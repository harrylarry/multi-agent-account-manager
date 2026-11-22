#!/usr/bin/env python3
"""事实意图图谱工具（graph.py）。

单文件、只用 Python 标准库。放在仓库 .muse-graph/tools/ 下，
Wally 和 Nova 跑的是同一个文件。协议见 ../PROTOCOL.md。

命令：
  graph.py validate [--dir DIR]   校验图（JSON、字段、引用、计数器、密钥扫描）
  graph.py render   [--dir DIR]   从 JSON 重新生成 GRAPH.md（确定性输出）
  graph.py check    [--dir DIR]   重渲染并与 GRAPH.md 对 diff（不同则退出 1）
  graph.py recall   [--dir DIR] QUERY...  词面搜索相关事实和意图（前 5 条）
  graph.py new      [--dir DIR] KIND ...  按计数器取号新建节点

DIR 缺省为本脚本所在的 .muse-graph/ 目录。
GRAPH.md 由 render 生成，禁止手改。
"""

import argparse
import difflib
import json
import os
import re
import sys
from datetime import datetime, timezone

VERSION = "1.0.0"

DEFAULT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

FACT_FIELDS = ["id", "title", "description", "created_at", "creator", "source_intent"]
INTENT_FIELDS = ["id", "title", "from", "to", "description", "creator", "status",
                 "worker", "requires_confirmation", "confirmed_at", "created_at",
                 "concluded_at"]
HINT_FIELDS = ["id", "content", "creator", "created_at"]
INTENT_STATUSES = ("open", "claimed", "done")

SECRET_PATTERNS = [
    ("hsurr 凭证", re.compile(r"hsurr:[A-Za-z0-9_\-]+")),
    ("GitHub PAT", re.compile(r"ghp_[A-Za-z0-9]{20,}|gho_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}")),
    ("API key 形值", re.compile(r"sk-[A-Za-z0-9]{10,}")),
    ("AWS Access Key", re.compile(r"AKIA[0-9A-Z]{16}")),
    ("键值对形凭证", re.compile(r"(?i)(api[_-]?key|password|passwd|token)\s*[:=]\s*\S{4,}")),
]

CJK_RUN_RE = re.compile(r"[\u4e00-\u9fff]+")
WORD_RE = re.compile(r"[a-z0-9_]+")
# 单字查询时排除虚词，避免"的"这类字匹配全部节点
CJK_STOPWORDS = set("的了在是和有不我你他她它这那那个为以对就都也还而与或但若如")


def now_iso():
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def parse_iso(s):
    if not isinstance(s, str):
        return False
    try:
        datetime.fromisoformat(s.replace("Z", "+00:00"))
        return True
    except ValueError:
        return False


def load_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def load_graph(graph_dir):
    """返回 (project, facts, intents, hints, errors)。

    facts/intents/hints 是 [(文件名, 节点)] 列表；errors 是加载期问题。
    """
    errors = []
    project = None
    ppath = os.path.join(graph_dir, "project.json")
    if not os.path.isfile(ppath):
        errors.append("project.json：文件缺失")
    else:
        try:
            project = load_json(ppath)
        except (OSError, ValueError) as e:
            errors.append("project.json：解析失败（%s）" % e)
        if project is not None and not isinstance(project, dict):
            errors.append("project.json：根不是对象")
            project = None

    def load_nodes(sub):
        nodes = []
        d = os.path.join(graph_dir, sub)
        if not os.path.isdir(d):
            return nodes
        for name in sorted(os.listdir(d)):
            if not name.endswith(".json"):
                continue
            path = os.path.join(d, name)
            try:
                node = load_json(path)
            except (OSError, ValueError) as e:
                errors.append("%s/%s：解析失败（%s）" % (sub, name, e))
                continue
            if not isinstance(node, dict):
                errors.append("%s/%s：根不是对象" % (sub, name))
                continue
            nodes.append((name, node))
        return nodes

    facts = load_nodes("facts")
    intents = load_nodes("intents")
    hints = load_nodes("hints")
    return project, facts, intents, hints, errors


def check_fields(loc, node, fields, errors):
    for f in fields:
        if f not in node:
            errors.append("%s：缺字段 %s" % (loc, f))


def check_nonempty_str(loc, node, field, errors):
    if field in node and (not isinstance(node[field], str) or not node[field].strip()):
        errors.append("%s：%s 为空或不是字符串" % (loc, field))


def check_iso_or_null(loc, node, field, errors):
    v = node.get(field)
    if v is not None and not parse_iso(v):
        errors.append("%s：%s 不是 ISO 时间" % (loc, field))


def cmd_validate(graph_dir):
    project, facts, intents, hints, errors = load_graph(graph_dir)
    fact_by_id = {}
    for name, n in facts:
        if isinstance(n.get("id"), str):
            fact_by_id.setdefault(n["id"], (name, n))
    intent_by_id = {}
    for name, n in intents:
        if isinstance(n.get("id"), str):
            intent_by_id.setdefault(n["id"], (name, n))

    # project.json
    if project is not None:
        for f in ["id", "repo", "goal", "status", "created_at", "counters"]:
            if f not in project:
                errors.append("project.json：缺字段 %s" % f)
        check_nonempty_str("project.json", project, "goal", errors)
        check_nonempty_str("project.json", project, "repo", errors)
        if "created_at" in project and not parse_iso(project.get("created_at")):
            errors.append("project.json：created_at 不是 ISO 时间")
        counters = project.get("counters")
        if isinstance(counters, dict):
            for k in ("fact", "intent", "hint"):
                v = counters.get(k)
                if not isinstance(v, int) or isinstance(v, bool) or v < 0:
                    errors.append("project.json：counters.%s 不是非负整数" % k)
        else:
            errors.append("project.json：counters 不是对象")

        def max_num(nodes, prefix):
            m = 0
            for _, n in nodes:
                nid = n.get("id")
                if isinstance(nid, str) and re.fullmatch(prefix + r"\d{3}", nid):
                    m = max(m, int(nid[1:]))
            return m

        if isinstance(counters, dict):
            for kind, nodes, prefix in (("fact", facts, "f"),
                                        ("intent", intents, "i"),
                                        ("hint", hints, "h")):
                c = counters.get(kind)
                m = max_num(nodes, prefix)
                if isinstance(c, int) and not isinstance(c, bool) and c != m:
                    errors.append("project.json：counters.%s 是 %d，实际最大编号是 %d" % (kind, c, m))

    # facts
    for name, n in facts:
        loc = "facts/" + name
        check_fields(loc, n, FACT_FIELDS, errors)
        nid = n.get("id")
        if isinstance(nid, str):
            if not re.fullmatch(r"f\d{3}", nid):
                errors.append("%s：id %s 格式不对，应为 fNNN" % (loc, nid))
            elif name != nid + ".json":
                errors.append("%s：文件名与 id %s 不一致" % (loc, nid))
        for f in ("title", "description", "creator"):
            check_nonempty_str(loc, n, f, errors)
        if "created_at" in n and not parse_iso(n.get("created_at")):
            errors.append("%s：created_at 不是 ISO 时间" % loc)
        si = n.get("source_intent")
        if si is not None and si not in intent_by_id:
            errors.append("%s：source_intent %s 不存在" % (loc, si))
        refs = n.get("refs")
        if refs is not None:
            if not isinstance(refs, dict):
                errors.append("%s：refs 不是对象" % loc)
            else:
                for k, v in refs.items():
                    if k in ("issue", "pr") and (not isinstance(v, int) or isinstance(v, bool)):
                        errors.append("%s：refs.%s 不是整数" % (loc, k))

    # intents
    for name, n in intents:
        loc = "intents/" + name
        check_fields(loc, n, INTENT_FIELDS, errors)
        nid = n.get("id")
        if isinstance(nid, str):
            if not re.fullmatch(r"i\d{3}", nid):
                errors.append("%s：id %s 格式不对，应为 iNNN" % (loc, nid))
            elif name != nid + ".json":
                errors.append("%s：文件名与 id %s 不一致" % (loc, nid))
        for f in ("title", "description", "creator"):
            check_nonempty_str(loc, n, f, errors)
        frm = n.get("from")
        if isinstance(frm, list):
            if not frm:
                errors.append("%s：from 为空，至少要有一个事实" % loc)
            for ref in frm:
                if ref not in fact_by_id:
                    errors.append("%s：from 引用 %s 不存在" % (loc, ref))
        elif "from" in n:
            errors.append("%s：from 不是列表" % loc)
        to = n.get("to")
        if to is not None and to not in fact_by_id:
            errors.append("%s：to 引用 %s 不存在" % (loc, to))
        status = n.get("status")
        if status is not None and status not in INTENT_STATUSES:
            errors.append("%s：status %s 不合法" % (loc, status))
        if to is not None and status != "done":
            errors.append("%s：to 已指向 %s 但 status 不是 done" % (loc, to))
        if status == "done" and to is None:
            errors.append("%s：status 是 done 但 to 为空" % loc)
        if "requires_confirmation" in n and not isinstance(n["requires_confirmation"], bool):
            errors.append("%s：requires_confirmation 不是布尔值" % loc)
        if n.get("worker") is not None and not isinstance(n.get("worker"), str):
            errors.append("%s：worker 不是字符串" % loc)
        for f in ("confirmed_at", "concluded_at"):
            check_iso_or_null(loc, n, f, errors)
        if "created_at" in n and not parse_iso(n.get("created_at")):
            errors.append("%s：created_at 不是 ISO 时间" % loc)
        # 双向一致：to 指向的 fact 的 source_intent 应指回来
        if to in fact_by_id and isinstance(nid, str):
            fname, fact = fact_by_id[to]
            if fact.get("source_intent") != nid:
                errors.append("facts/%s：source_intent 应为 %s（被 %s 指向）" % (fname, nid, nid))

    # hints
    for name, n in hints:
        loc = "hints/" + name
        check_fields(loc, n, HINT_FIELDS, errors)
        nid = n.get("id")
        if isinstance(nid, str):
            if not re.fullmatch(r"h\d{3}", nid):
                errors.append("%s：id %s 格式不对，应为 hNNN" % (loc, nid))
            elif name != nid + ".json":
                errors.append("%s：文件名与 id %s 不一致" % (loc, nid))
        check_nonempty_str(loc, n, "content", errors)
        check_nonempty_str(loc, n, "creator", errors)
        if "created_at" in n and not parse_iso(n.get("created_at")):
            errors.append("%s：created_at 不是 ISO 时间" % loc)

    # 密钥扫描
    blobs = []
    if project is not None:
        blobs.append(("project.json", json.dumps(project, ensure_ascii=False)))
    for sub, nodes in (("facts", facts), ("intents", intents), ("hints", hints)):
        for name, n in nodes:
            blobs.append((sub + "/" + name, json.dumps(n, ensure_ascii=False)))
    for loc, blob in blobs:
        for label, pat in SECRET_PATTERNS:
            if pat.search(blob):
                errors.append("%s：可能含密钥（%s），请检查后删除" % (loc, label))

    if errors:
        for e in errors:
            print("ERROR " + e)
        print("校验失败：%d 个问题" % len(errors))
        return 1
    print("校验通过：%d 个事实，%d 个 intent，%d 条提示" % (len(facts), len(intents), len(hints)))
    return 0


def refs_suffix(node):
    parts = []
    refs = node.get("refs")
    if isinstance(refs, dict):
        if refs.get("issue") is not None:
            parts.append("Issue #%s" % refs["issue"])
        if refs.get("pr") is not None:
            parts.append("PR #%s" % refs["pr"])
    if node.get("issue") is not None:
        parts.append("Issue #%s" % node["issue"])
    if parts:
        return "（%s）" % "、".join(parts)
    return ""


def build_graph_md(project, facts, intents, hints):
    repo = project.get("repo", "") if project else ""
    short = repo.split("/")[-1] if repo else ""
    goal = project.get("goal", "") if project else ""
    fact_list = sorted((n for _, n in facts if isinstance(n.get("id"), str)),
                       key=lambda n: n["id"])
    intent_list = sorted((n for _, n in intents if isinstance(n.get("id"), str)),
                         key=lambda n: n["id"])
    hint_list = sorted((n for _, n in hints if isinstance(n.get("id"), str)),
                       key=lambda n: n["id"])
    n_open = sum(1 for n in intent_list if n.get("status") == "open")
    n_claimed = sum(1 for n in intent_list if n.get("status") == "claimed")
    n_done = sum(1 for n in intent_list if n.get("status") == "done")

    lines = []
    lines.append("# 图谱摘要（%s）" % short)
    lines.append("")
    lines.append("目标：%s" % goal)
    lines.append("状态：%d 个事实，%d 个 intent（%d 待做 / %d 进行中 / %d 已完成），%d 条提示" % (
        len(fact_list), len(intent_list), n_open, n_claimed, n_done, len(hint_list)))
    lines.append("协议：见 PROTOCOL.md")
    lines.append("")
    lines.append("## 前沿（可干）")
    lines.append("")
    frontier = [n for n in intent_list if n.get("status") in ("open", "claimed")]
    if frontier:
        for n in frontier:
            extra = ""
            if n.get("requires_confirmation") and not n.get("confirmed_at"):
                extra = "（需用户确认）"
            lines.append("- %s：%s%s%s" % (n["id"], n.get("title", ""), refs_suffix(n), extra))
    else:
        lines.append("（无）")
    lines.append("")
    lines.append("## 最近事实（最新 8 条）")
    lines.append("")
    if fact_list:
        for n in sorted(fact_list, key=lambda n: n["id"], reverse=True)[:8]:
            lines.append("- %s：%s%s" % (n["id"], n.get("title", ""), refs_suffix(n)))
    else:
        lines.append("（无）")
    lines.append("")
    lines.append("## 最近完成（最新 4 条）")
    lines.append("")
    done = [n for n in intent_list if n.get("status") == "done"]
    if done:
        for n in sorted(done, key=lambda n: n["id"], reverse=True)[:4]:
            lines.append("- %s → %s：%s%s" % (n["id"], n.get("to") or "?", n.get("title", ""), refs_suffix(n)))
    else:
        lines.append("（无）")
    lines.append("")
    lines.append("## 提示")
    lines.append("")
    if hint_list:
        for n in hint_list:
            lines.append("- %s：%s" % (n["id"], n.get("content", "")))
    else:
        lines.append("（无）")
    return "\n".join(lines) + "\n"


def cmd_render(graph_dir):
    project, facts, intents, hints, errors = load_graph(graph_dir)
    if errors or project is None:
        for e in errors:
            print("ERROR " + e)
        print("图加载失败，先修好再渲染")
        return 1
    text = build_graph_md(project, facts, intents, hints)
    path = os.path.join(graph_dir, "GRAPH.md")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print("已生成 %s（%d 个事实，%d 个 intent，%d 条提示）" % (path, len(facts), len(intents), len(hints)))
    return 0


def cmd_check(graph_dir):
    project, facts, intents, hints, errors = load_graph(graph_dir)
    if errors or project is None:
        for e in errors:
            print("ERROR " + e)
        print("图加载失败，先修好再检查")
        return 1
    path = os.path.join(graph_dir, "GRAPH.md")
    if not os.path.isfile(path):
        print("GRAPH.md 缺失，请跑 render 生成")
        return 1
    with open(path, encoding="utf-8") as f:
        current = f.read()
    rendered = build_graph_md(project, facts, intents, hints)
    if current == rendered:
        print("GRAPH.md 已同步")
        return 0
    print("GRAPH.md 已过期，请跑 render 同步。差异：")
    diff = difflib.unified_diff(current.splitlines(keepends=True),
                                rendered.splitlines(keepends=True),
                                fromfile="GRAPH.md（当前）", tofile="GRAPH.md（渲染）")
    sys.stdout.writelines(diff)
    return 1


def tokenize(text):
    """词面切分：ASCII 按词，中文按二元字串（单字切分太 noisy，“的”会匹配全部节点）。"""
    if not isinstance(text, str):
        return set()
    text = text.lower()
    tokens = set(WORD_RE.findall(text))
    for run in CJK_RUN_RE.findall(text):
        if len(run) == 1:
            if run not in CJK_STOPWORDS:
                tokens.add(run)
        else:
            for i in range(len(run) - 1):
                tokens.add(run[i:i + 2])
    return tokens


def cmd_recall(graph_dir, query_parts):
    query = " ".join(query_parts).strip()
    if not query:
        print("请给出搜索词")
        return 2
    project, facts, intents, hints, errors = load_graph(graph_dir)
    if errors:
        for e in errors:
            print("ERROR " + e)
        print("图加载失败，先修好再搜索")
        return 1
    qtokens = tokenize(query)
    qlow = query.lower()
    scored = []
    for _, n in list(facts) + list(intents):
        nid = n.get("id", "")
        title = n.get("title", "") or ""
        desc = n.get("description", "") or ""
        doc = n.get("doc", "") or ""
        score = 0
        t_title = tokenize(title)
        t_desc = tokenize(desc)
        t_doc = tokenize(doc)
        for t in qtokens:
            if t in t_title:
                score += 2
            if t in t_desc:
                score += 1
            if t in t_doc:
                score += 1
        if len(query) >= 2:
            if qlow in title.lower():
                score += 4
            if qlow in desc.lower():
                score += 2
            if qlow in doc.lower():
                score += 1
        if qlow == str(nid).lower():
            score += 10
        if score > 0:
            scored.append((score, nid, n))
    scored.sort(key=lambda x: (-x[0], str(x[1])))
    if not scored:
        print("没有匹配的节点")
        return 0
    for score, nid, n in scored[:5]:
        desc = (n.get("description") or "").replace("\n", " ")
        if len(desc) > 60:
            desc = desc[:60] + "…"
        print("%s %s —— %s" % (nid, n.get("title", ""), desc))
    return 0


def cmd_new(graph_dir, kind, args):
    if kind in ("fact", "intent") and (not args.title or not args.description):
        print("fact/intent 需要 --title 和 --description")
        return 2
    if kind == "hint" and not args.content:
        print("hint 需要 --content")
        return 2
    project, facts, intents, hints, errors = load_graph(graph_dir)
    if errors or project is None:
        for e in errors:
            print("ERROR " + e)
        print("图加载失败，先修好再新建")
        return 1
    counters = project.get("counters") or {}
    prefix = {"fact": "f", "intent": "i", "hint": "h"}[kind]
    sub = {"fact": "facts", "intent": "intents", "hint": "hints"}[kind]
    nxt = int(counters.get(kind, 0)) + 1
    nid = "%s%03d" % (prefix, nxt)
    path = os.path.join(graph_dir, sub, nid + ".json")
    if os.path.exists(path):
        print("文件已存在：%s（计数器与实际不符，先跑 validate）" % path)
        return 1
    node = {"id": nid, "created_at": now_iso(), "creator": args.creator}
    if kind == "fact":
        if args.source_intent:
            intent_ids = {n.get("id") for _, n in intents}
            if args.source_intent not in intent_ids:
                print("source_intent %s 不存在" % args.source_intent)
                return 1
        node["title"] = args.title
        node["description"] = args.description
        node["source_intent"] = args.source_intent
    elif kind == "intent":
        if not args.from_facts:
            print("intent 必须有 --from，至少引用一个事实")
            return 1
        frm = [x.strip() for x in args.from_facts.split(",") if x.strip()]
        fact_ids = {n.get("id") for _, n in facts}
        for ref in frm:
            if ref not in fact_ids:
                print("from 引用 %s 不存在" % ref)
                return 1
        node["title"] = args.title
        node["description"] = args.description
        node["from"] = frm
        node["to"] = None
        node["status"] = "open"
        node["worker"] = None
        node["requires_confirmation"] = bool(args.requires_confirmation)
        node["confirmed_at"] = None
        node["concluded_at"] = None
        if args.issue is not None:
            node["issue"] = args.issue
    else:
        node["content"] = args.content
    os.makedirs(os.path.join(graph_dir, sub), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(node, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")
    counters[kind] = nxt
    project["counters"] = counters
    with open(os.path.join(graph_dir, "project.json"), "w", encoding="utf-8") as f:
        json.dump(project, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")
    print("已创建 %s（id %s），计数器已更新。记得跑 render 同步 GRAPH.md，再跑 validate 自查。" % (path, nid))
    return 0


def main(argv=None):
    p = argparse.ArgumentParser(prog="graph.py", description="事实意图图谱工具（协议见 PROTOCOL.md）")
    p.add_argument("--version", action="version", version="graph.py " + VERSION)
    sub = p.add_subparsers(dest="cmd", required=True)

    for name, help_text in (("validate", "校验图"),
                            ("render", "重新生成 GRAPH.md"),
                            ("check", "检查 GRAPH.md 是否与图同步")):
        sp = sub.add_parser(name, help=help_text)
        sp.add_argument("--dir", default=DEFAULT_DIR, help="图目录（缺省为本脚本所在的 .muse-graph/）")

    sp = sub.add_parser("recall", help="词面搜索相关事实和意图")
    sp.add_argument("--dir", default=DEFAULT_DIR)
    sp.add_argument("query", nargs="+", help="搜索词")

    sp = sub.add_parser("new", help="新建一个节点")
    sp.add_argument("--dir", default=DEFAULT_DIR)
    sp.add_argument("kind", choices=["fact", "intent", "hint"])
    sp.add_argument("--title")
    sp.add_argument("--description")
    sp.add_argument("--content", help="hint 内容")
    sp.add_argument("--creator", required=True, help="记录人（wally / nova / 其他 worker 名）")
    sp.add_argument("--from", dest="from_facts", help="intent 依赖的 fact id，逗号分隔")
    sp.add_argument("--source-intent", help="fact 的来源 intent id")
    sp.add_argument("--issue", type=int, help="对应任务单 Issue 号")
    sp.add_argument("--requires-confirmation", action="store_true")

    args = p.parse_args(argv)
    if args.cmd == "validate":
        return cmd_validate(args.dir)
    if args.cmd == "render":
        return cmd_render(args.dir)
    if args.cmd == "check":
        return cmd_check(args.dir)
    if args.cmd == "recall":
        return cmd_recall(args.dir, args.query)
    if args.cmd == "new":
        return cmd_new(args.dir, args.kind, args)
    return 2


if __name__ == "__main__":
    sys.exit(main())
