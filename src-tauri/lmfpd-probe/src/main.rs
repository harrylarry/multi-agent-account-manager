//! 最小实验：只验证两件事
//! 1) `lmfpd` 能否为 aarch64-apple-ios 编译；
//! 2) 能否用本地参考库对「已经采集到的回答」做离线评分（不调用 SDK 的发送客户端）。
//!
//! 用法：
//!   lmfpd-probe --data <dir> --outputs <outputs.json>   # 本地参考库（离线）
//!   lmfpd-probe --default --outputs <outputs.json>       # 官方默认源（带缓存）
//!   outputs.json = [{"text": "...", "expectedCount": 305}, ...]

use std::path::PathBuf;

use lmfpd::{Detector, LoadOptions, Output};

fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let mut data: Option<PathBuf> = None;
  let mut use_default = false;
  let mut outputs_path: Option<PathBuf> = None;
  let mut i = 0;
  while i < args.len() {
    match args[i].as_str() {
      "--data" => data = args.get(i + 1).map(PathBuf::from),
      "--default" => {
        // 没有参数，不能 i += 2。
        use_default = true;
        i += 1;
        continue;
      }
      "--outputs" => outputs_path = args.get(i + 1).map(PathBuf::from),
      other => {
        eprintln!("unknown arg: {other}");
        std::process::exit(2);
      }
    }
    i += 2;
  }
  // 两种来源：本地目录（离线、可复现）或官方默认源（首次下载 + 缓存）。
  let options = match (&data, use_default) {
    (Some(d), _) => LoadOptions {
      sources: vec![d.to_string_lossy().to_string()],
      ..LoadOptions::default()
    },
    (None, true) => LoadOptions::default(),
    (None, false) => {
      eprintln!("usage: lmfpd-probe (--data <dir> | --default) [--outputs <outputs.json>]");
      std::process::exit(2);
    }
  };
  let detector = match Detector::load(&options) {
    Ok(d) => d,
    Err(e) => {
      eprintln!("load failed: {e}");
      std::process::exit(1);
    }
  };
  println!("loaded built_at={}", detector.built_at());

  let outputs: Vec<Output> = match outputs_path {
    Some(p) => {
      let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| {
        eprintln!("read {} failed: {e}", p.display());
        std::process::exit(1);
      });
      let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        eprintln!("parse outputs failed: {e}");
        std::process::exit(1);
      });
      v.as_array()
        .map(|arr| {
          arr
            .iter()
            .map(|o| Output {
              text: o.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
              expected_count: o
                .get("expectedCount")
                .or_else(|| o.get("expected_count"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0),
            })
            .collect()
        })
        .unwrap_or_default()
    }
    None => Vec::new(),
  };

  match detector.analyze(&outputs, false) {
    Ok(a) => {
      // 只打印判定与候选，不编造：results 为空就是空。
      println!(
        "decision={:?} prediction={} family={} used={} cands={}",
        a.decision,
        a.prediction,
        a.family_prediction,
        a.used_outputs,
        a.results.len()
      );
      for (i, r) in a.results.iter().take(3).enumerate() {
        println!("  #{} {} ({}) score={:.4} p={:?}", i + 1, r.display_name, r.model, r.score, r.probability);
      }
      println!("evidence={} {}", a.evidence.label, a.evidence.reason);
    }
    Err(e) => {
      eprintln!("analyze failed: {e}");
      std::process::exit(1);
    }
  }
}
