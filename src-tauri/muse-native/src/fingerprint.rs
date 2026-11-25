//! #18 模型指纹：评分编排层（Issue #14 的 C5 切片）。
//!
//! 本模块只做「拿到已经采集好的三条探针回答之后」的事：
//! 1. 把采集结果与对应的 challenge（期望计数）配对、校验、判定有效样本；
//! 2. 加载 lmfpd 参考库（首次下载后由 SDK 缓存），本地调用 `Detector::analyze`；
//! 3. 把 SDK 的 `Analysis` 映射成前端可直接展示的候选 / 得分 / 可信度；
//! 4. 严格区分「已判定」「部分排序」「不可判定」，绝不把 `Unscorable` /
//!    `NotModel` 冒充有效识别结果。
//!
//! 采集（隔离会话里发 3 条探针、订阅收正文）在 collect.rs / chat_send.rs，
//! App 接线与 Tauri 命令在 src-tauri/src/muse_native.rs 与 lib.rs。
//! 本模块不含任何联网发送逻辑；lmfpd 只用本地评分，不用它的联网 detect。
//!
//! 安全：探针内容是无语义整数计数（generate_challenges 生成，计数 292–332），
//! 不含隐私；评分在本地做，不上传第三方。

use std::fmt;

use lmfpd::{Analysis, Decision, Detector, LoadOptions, Output, ProbabilityStatus};
use serde::Serialize;

/// 一次完整指纹评估所需的目标样本数。lmfpd 的三条有效回答才给校准置信度；
/// 少于三条只给候选排名（Partial）或不可判定（Unscorable）。
pub const REQUIRED_SAMPLES: usize = 3;

/// 与 lmfpd `features::minimum_numbers` 一致：每条回答至少要解析出的数字个数。
/// 复刻为本地常量，仅用于「有效样本」预筛选与前端诊断；最终判定仍由 lmfpd
/// 的 `Analysis.diagnostics[].accepted` 与 `used_outputs` 决定。
pub fn minimum_numbers(expected: u64) -> u64 {
  80.max((expected as f64 * 0.55).ceil() as u64)
}

/// 一条要发给隔离会话的探针题。内容是无语义整数计数，不含隐私。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeChallenge {
  pub id: String,
  pub expected_count: u64,
  pub prompt: String,
}

/// 用 lmfpd 真实生成器产生三条探针题（计数 292–332，语言/句式随机）。
pub fn generate_probes() -> Result<Vec<ProbeChallenge>, FingerprintError> {
  let challenges = lmfpd::generate_challenges(REQUIRED_SAMPLES)
    .map_err(|e| FingerprintError::ProbeGeneration(e.to_string()))?;
  Ok(challenges
    .into_iter()
    .map(|c| ProbeChallenge {
      id: c.id,
      expected_count: c.expected_count,
      prompt: c.prompt,
    })
    .collect())
}

/// 一条采集结果：正文 + 它对应的 challenge 期望计数。
#[derive(Debug, Clone)]
pub struct Sample {
  /// 采集到的助手正文（含增量拼接；可能为空）。
  pub text: String,
  /// 该探针要求数到的整数（292–332）。
  pub expected_count: u64,
}

/// 单条样本的校验诊断（只读、可展示；不含正文）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleCheck {
  pub expected_count: u64,
  pub parsed_numbers: usize,
  pub minimum_numbers: u64,
  pub accepted: bool,
}

/// 校验一条样本是否达到 lmfpd 的最低数字数阈值。
pub fn check_sample(text: &str, expected_count: u64) -> SampleCheck {
  let parsed = lmfpd::parse_numbers(text).len();
  let min = minimum_numbers(expected_count);
  SampleCheck {
    expected_count,
    parsed_numbers: parsed,
    minimum_numbers: min,
    accepted: expected_count > 0 && parsed as u64 >= min,
  }
}

/// 把采集样本转成 lmfpd `Output`。不修改正文：lmfpd 会自行截断多余数字、
/// 按 own diagnostics 判定有效样本，我们不做任何"优化"或伪造。
pub fn to_outputs(samples: &[Sample]) -> Vec<Output> {
  samples
    .iter()
    .map(|s| Output {
      text: s.text.clone(),
      expected_count: s.expected_count,
    })
    .collect()
}

/// 我们对外的判定状态。刻意比 lmfpd 的 `Decision` 更粗，避免前端误用：
/// - `scored`：三条有效样本 + 校准置信度，给出候选排名；
/// - `partial`：部分样本，只给候选排名，不给置信度，不算"有效识别"；
/// - `indeterminate`：不可判定（样本不足 / 非模型回复），没有候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
  Scored,
  Partial,
  Indeterminate,
}

impl Verdict {
  pub fn as_str(self) -> &'static str {
    match self {
      Verdict::Scored => "scored",
      Verdict::Partial => "partial",
      Verdict::Indeterminate => "indeterminate",
    }
  }

  /// 是否算"有效识别结果"。只有 Scored 才是；Partial 是弱信号，Indeterminate 不是。
  pub fn is_identification(self) -> bool {
    matches!(self, Verdict::Scored)
  }
}

/// 单个候选模型。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
  pub model: String,
  pub display_name: String,
  pub family: String,
  pub family_name: String,
  pub score: f64,
  /// 闭集校准概率；只有 Scored + ReferenceCalibrated 时才为 Some。
  pub probability: Option<f64>,
}

/// 一次评分的完整输出。所有字段前端契约稳定，Unscorable 时 candidates 为空、
/// prediction 为空、verdict=indeterminate。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintResult {
  pub verdict: Verdict,
  /// 是否可作为有效识别结果（等价 verdict==scored）。
  pub identification: bool,
  pub prediction: String,
  pub prediction_name: String,
  pub family_prediction: String,
  pub family_prediction_name: String,
  pub candidates: Vec<Candidate>,
  /// 实际参与评分的有效样本数（lmfpd used_outputs）。
  pub used_samples: usize,
  pub required_samples: usize,
  /// 是否有校准置信度。
  pub calibrated: bool,
  pub probability: Option<f64>,
  pub probability_status: String,
  pub evidence_label: String,
  pub evidence_reason: String,
  /// 参考库构建标识（built_at），随结果存档以便追溯。
  pub reference_built_at: String,
  pub reference_sha256: Option<String>,
  pub method: String,
  /// 每条样本的解析诊断，不含正文。
  pub samples: Vec<SampleCheck>,
}

impl FingerprintResult {
  /// 样本不足 / 请求失败时的"不可判定"结果：没有任何候选，也不给置信度。
  pub fn indeterminate(reason: impl Into<String>, reference_built_at: String) -> Self {
    Self {
      verdict: Verdict::Indeterminate,
      identification: false,
      prediction: String::new(),
      prediction_name: String::new(),
      family_prediction: String::new(),
      family_prediction_name: String::new(),
      candidates: Vec::new(),
      used_samples: 0,
      required_samples: REQUIRED_SAMPLES,
      calibrated: false,
      probability: None,
      probability_status: "unavailable".to_string(),
      evidence_label: "Not enough valid samples".to_string(),
      evidence_reason: reason.into(),
      reference_built_at,
      reference_sha256: None,
      method: String::new(),
      samples: Vec::new(),
    }
  }
}

/// 把 lmfpd 的 `Analysis` 映射成前端结果。纯函数、可单测。
///
/// 关键规则：只有 `Decision::Scored` 才算有效识别；`Partial` 保留候选排名但
/// 去掉置信度、不算识别；`Unscorable` / `NotModel` 不产出任何候选与预测。
pub fn interpret(analysis: &Analysis, samples: &[SampleCheck], reference_built_at: &str) -> FingerprintResult {
  let probability_status = match analysis.probability_status {
    ProbabilityStatus::ReferenceCalibrated => "referenceCalibrated",
    ProbabilityStatus::Unavailable => "unavailable",
  };

  let (verdict, probability, calibrated) = match analysis.decision {
    Decision::Scored => (
      Verdict::Scored,
      analysis.probability,
      analysis.probability_status == ProbabilityStatus::ReferenceCalibrated,
    ),
    // 部分样本：保留候选排名，但置信度不可用，不算有效识别。
    Decision::Partial => (Verdict::Partial, None, false),
    // 明确不可判定：清空候选与预测，绝不把 Unscorable / NotModel 当识别。
    Decision::Unscorable | Decision::NotModel => (Verdict::Indeterminate, None, false),
  };

  let keep = verdict != Verdict::Indeterminate;
  // 只有 Scored + ReferenceCalibrated 才保留闭集概率；Partial 即便 SDK 给出了
  // probability 也不能展示（样本不足，校准不成立），一律清空。
  let calibrated = calibrated && verdict == Verdict::Scored;
  let probability = if calibrated { probability } else { None };
  let candidates = if keep {
    analysis
      .results
      .iter()
      .map(|r| Candidate {
        model: r.model.clone(),
        display_name: r.display_name.clone(),
        family: r.family.clone(),
        family_name: r.family_name.clone(),
        score: r.score,
        probability: if calibrated { r.probability } else { None },
      })
      .collect()
  } else {
    Vec::new()
  };

  FingerprintResult {
    verdict,
    identification: verdict.is_identification(),
    prediction: if keep { analysis.prediction.clone() } else { String::new() },
    prediction_name: if keep { analysis.prediction_name.clone() } else { String::new() },
    family_prediction: if keep { analysis.family_prediction.clone() } else { String::new() },
    family_prediction_name: if keep {
      analysis.family_prediction_name.clone()
    } else {
      String::new()
    },
    candidates,
    used_samples: analysis.used_outputs,
    required_samples: REQUIRED_SAMPLES,
    calibrated,
    probability,
    probability_status: probability_status.to_string(),
    evidence_label: analysis.evidence.label.clone(),
    evidence_reason: analysis.evidence.reason.clone(),
    reference_built_at: reference_built_at.to_string(),
    reference_sha256: Some(analysis.model_version.base_sha256.clone()),
    method: analysis.method.clone(),
    samples: samples.to_vec(),
  }
}

/// 评分失败的类型。采集侧（请求失败/无登录态）由 App 层映射为相同对外形状，
/// 这里只覆盖参考库与评分本身。
#[derive(Debug)]
pub enum FingerprintError {
  /// 生成探针题失败。
  ProbeGeneration(String),
  /// 样本为零或超过 3 条，无法交给 lmfpd。
  InvalidSampleCount(usize),
  /// 参考库加载/下载失败。
  ReferenceBank(String),
  /// lmfpd analyze 失败。
  Scoring(String),
}

impl fmt::Display for FingerprintError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      FingerprintError::ProbeGeneration(e) => write!(f, "生成探针失败: {e}"),
      FingerprintError::InvalidSampleCount(n) => {
        write!(f, " fingerprints 需要 1–3 条样本，收到 {n} 条")
      }
      FingerprintError::ReferenceBank(e) => write!(f, "参考库加载失败: {e}"),
      FingerprintError::Scoring(e) => write!(f, "评分失败: {e}"),
    }
  }
}

impl std::error::Error for FingerprintError {}

/// 已加载的 lmfpd 参考库 + 检测器。可缓存复用（analyze 是 &self）。
/// 用 `LoadOptions::default()`：来源为官方 URL（含国内镜像），缓存到 OS 缓存目录，
/// 24 小时后后台刷新；"首次下载后缓存"由 SDK 保证。
pub struct FingerprintDetector {
  detector: Detector,
  built_at: String,
}

impl FingerprintDetector {
  /// 加载参考库并缓存到 OS 缓存目录；首次会联网下载。
  pub fn load() -> Result<Self, FingerprintError> {
    let options = LoadOptions::default();
    Detector::load(&options)
      .map(|detector| {
        let built_at = detector.built_at().to_string();
        Self { detector, built_at }
      })
      .map_err(|e| FingerprintError::ReferenceBank(e.to_string()))
  }

  /// 从本地目录加载（离线/可复现实验用），不走缓存。
  pub fn load_from_dir(dir: impl Into<String>) -> Result<Self, FingerprintError> {
    let options = LoadOptions {
      sources: vec![dir.into()],
      ..LoadOptions::default()
    };
    Detector::load(&options)
      .map(|detector| {
        let built_at = detector.built_at().to_string();
        Self { detector, built_at }
      })
      .map_err(|e| FingerprintError::ReferenceBank(e.to_string()))
  }

  pub fn built_at(&self) -> &str {
    &self.built_at
  }

  /// 用真实算法评分。`strict=false`：1–2 条样本只给 Partial 排名，
  /// 三条有效样本才给校准置信度（Scored）。
  pub fn score(&self, samples: &[Sample]) -> Result<FingerprintResult, FingerprintError> {
    if samples.is_empty() || samples.len() > REQUIRED_SAMPLES {
      return Err(FingerprintError::InvalidSampleCount(samples.len()));
    }
    let checks: Vec<SampleCheck> = samples
      .iter()
      .map(|s| check_sample(&s.text, s.expected_count))
      .collect();
    let outputs = to_outputs(samples);
    let analysis = self
      .detector
      .analyze(&outputs, false)
      .map_err(|e| FingerprintError::Scoring(e.to_string()))?;
    Ok(interpret(&analysis, &checks, &self.built_at))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use lmfpd::{Analysis, AnalysisResult, Evidence, ModelVersion};

  fn analysis(decision: Decision, used: usize) -> Analysis {
    Analysis {
      decision,
      prediction: "gpt-x".to_string(),
      prediction_name: "GPT X".to_string(),
      family_prediction: "gpt".to_string(),
      family_prediction_name: "GPT".to_string(),
      results: vec![AnalysisResult {
        model: "gpt-x".to_string(),
        display_name: "GPT X".to_string(),
        family: "gpt".to_string(),
        family_name: "GPT".to_string(),
        score: 0.9,
        probability: Some(0.81),
      }],
      ranking_score: Some(0.9),
      probability: Some(0.81),
      probability_status: ProbabilityStatus::ReferenceCalibrated,
      probability_scope: None,
      calibration: None,
      used_outputs: used,
      diagnostics: Vec::new(),
      sequence: None,
      method: "shared-detector-v2".to_string(),
      model_version: ModelVersion {
        base_sha256: "abc".to_string(),
      },
      evidence: Evidence {
        insufficient: false,
        label: "Calibrated ranking".to_string(),
        reason: "ok".to_string(),
        threshold: None,
        method: "reference-calibrated-ranking".to_string(),
      },
    }
  }

  fn checks() -> Vec<SampleCheck> {
    vec![SampleCheck {
      expected_count: 300,
      parsed_numbers: 300,
      minimum_numbers: minimum_numbers(300),
      accepted: true,
    }]
  }

  #[test]
  fn generate_probes_returns_three_distinct_counts() {
    let probes = generate_probes().expect("generate");
    assert_eq!(probes.len(), REQUIRED_SAMPLES);
    let mut counts: Vec<u64> = probes.iter().map(|p| p.expected_count).collect();
    counts.sort();
    counts.dedup();
    assert_eq!(counts.len(), REQUIRED_SAMPLES, "计数必须互不相同");
    for p in &probes {
      assert!((292..=332).contains(&p.expected_count));
      assert!(!p.prompt.is_empty());
      assert!(!p.id.is_empty());
    }
  }

  #[test]
  fn minimum_numbers_matches_lmfpd() {
    assert_eq!(minimum_numbers(300), 165);
    assert_eq!(minimum_numbers(10), 80); // 80 下限
    assert_eq!(minimum_numbers(100), 80);
  }

  #[test]
  fn check_sample_uses_parse_threshold() {
    // parse_numbers 只收 1..=355 的连续数字段。
    let good = (1..=300).map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
    let c = check_sample(&good, 300);
    assert!(c.accepted, "{:?}", c);
    assert_eq!(c.parsed_numbers, 300);

    let bad = check_sample("1, 2, 3", 300);
    assert!(!bad.accepted);
    assert_eq!(bad.parsed_numbers, 3);
  }

  #[test]
  fn to_outputs_keeps_text_and_expected_count() {
    let samples = vec![
      Sample { text: "a".into(), expected_count: 300 },
      Sample { text: "b".into(), expected_count: 310 },
    ];
    let outs = to_outputs(&samples);
    assert_eq!(outs.len(), 2);
    assert_eq!(outs[0].text, "a");
    assert_eq!(outs[0].expected_count, 300);
    assert_eq!(outs[1].expected_count, 310);
  }

  #[test]
  fn scored_is_identification_with_probability() {
    let r = interpret(&analysis(Decision::Scored, 3), &checks(), "2026-01-01");
    assert_eq!(r.verdict, Verdict::Scored);
    assert!(r.identification);
    assert!(r.calibrated);
    assert_eq!(r.probability, Some(0.81));
    assert_eq!(r.candidates.len(), 1);
    assert_eq!(r.prediction, "gpt-x");
    assert_eq!(r.reference_built_at, "2026-01-01");
  }

  #[test]
  fn partial_keeps_ranking_but_no_confidence_and_not_identification() {
    let r = interpret(&analysis(Decision::Partial, 2), &checks(), "b");
    assert_eq!(r.verdict, Verdict::Partial);
    assert!(!r.identification);
    assert!(!r.calibrated);
    assert!(r.probability.is_none());
    // 候选排名保留，供人工参考；但概率一律清空。
    assert_eq!(r.candidates.len(), 1);
    assert!(r.candidates[0].probability.is_none());
  }

  #[test]
  fn unscorable_is_never_an_identification() {
    let r = interpret(&analysis(Decision::Unscorable, 1), &checks(), "b");
    assert_eq!(r.verdict, Verdict::Indeterminate);
    assert!(!r.identification);
    assert!(!r.calibrated);
    assert!(r.candidates.is_empty());
    assert!(r.prediction.is_empty());
    assert!(r.probability.is_none());
  }

  #[test]
  fn not_model_is_never_an_identification() {
    let r = interpret(&analysis(Decision::NotModel, 3), &checks(), "b");
    assert_eq!(r.verdict, Verdict::Indeterminate);
    assert!(!r.identification);
    assert!(r.candidates.is_empty());
    assert!(r.prediction.is_empty());
  }

  #[test]
  fn indeterminate_factory_has_no_candidates() {
    let r = FingerprintResult::indeterminate("not enough", "b".to_string());
    assert_eq!(r.verdict, Verdict::Indeterminate);
    assert!(r.candidates.is_empty());
    assert_eq!(r.required_samples, REQUIRED_SAMPLES);
    assert!(!r.identification);
  }

  #[test]
  fn uncalibrated_scored_drops_probability() {
    let mut a = analysis(Decision::Scored, 3);
    a.probability_status = ProbabilityStatus::Unavailable;
    a.probability = None;
    let r = interpret(&a, &checks(), "b");
    // 仍是 Scored（三条有效样本的完整排名），但缺校准数据 → 无置信度。
    // 排名本身依然是识别结果，只是不能声称有校准概率。
    assert_eq!(r.verdict, Verdict::Scored);
    assert!(r.identification);
    assert!(!r.calibrated);
    assert!(r.probability.is_none());
  }
}
