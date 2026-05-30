//! Prompt complexity classifier — ported from FreeRouter (MIT).
//!
//! Scores a prompt across 14 weighted dimensions and maps the aggregate
//! to one of four tiers in < 1 ms.
//!
//! ## Original
//! <https://github.com/openfreerouter/freerouter> (forked from ClawRouter, MIT)
//! The JS classifier was itself ported from the ClawRouter Rust codebase.

use serde::{Deserialize, Serialize};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Four routing tiers.  REASONING is distinct from COMPLEX because reasoning
/// tasks need models with explicit chain-of-thought support (deepseek-reasoner,
/// o3, qwen3 thinking mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PromptTier {
    Simple,
    Medium,
    Complex,
    Reasoning,
}

impl PromptTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Simple    => "SIMPLE",
            Self::Medium    => "MEDIUM",
            Self::Complex   => "COMPLEX",
            Self::Reasoning => "REASONING",
        }
    }
}

/// Output of the classifier.
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub score: f64,
    pub tier: Option<PromptTier>,
    pub confidence: f64,
    pub signals: Vec<String>,
    pub agentic_score: f64,
}

/// A mode override extracted from a prompt prefix — the prefix is stripped.
#[derive(Debug, Clone)]
pub struct ModeOverride {
    pub tier: PromptTier,
    pub cleaned_prompt: String,
}

// ─── Keyword lists (verbatim from FreeRouter upstream) ────────────────────────

const CODE_KEYWORDS: &[&str] = &[
    "function", "class", "import", "def", "SELECT", "async", "await",
    "const", "let", "var", "return", "```",
    "函数", "类", "导入", "定义", "查询", "异步", "等待", "常量", "变量", "返回",
    "関数", "クラス", "インポート", "非同期", "定数", "変数",
    "функция", "класс", "импорт", "определ", "запрос", "асинхронный",
    "ожидать", "константа", "переменная", "вернуть",
    "funktion", "klasse", "importieren", "definieren", "abfrage",
    "asynchron", "erwarten", "konstante", "variable", "zurückgeben",
];
const REASONING_KEYWORDS: &[&str] = &[
    "prove", "theorem", "derive", "step by step", "chain of thought",
    "formally", "mathematical", "proof", "logically",
    "证明", "定理", "推导", "逐步", "思维链", "形式化", "数学", "逻辑",
    "証明", "定理", "導出", "ステップバイステップ", "論理的",
    "доказать", "докажи", "доказательств", "теорема", "вывести",
    "шаг за шагом", "пошагово", "поэтапно", "цепочка рассуждений",
    "рассуждени", "формально", "математически", "логически",
    "beweisen", "beweis", "ableiten", "schritt für schritt",
    "gedankenkette", "formal", "mathematisch", "logisch",
];
const SIMPLE_KEYWORDS: &[&str] = &[
    "what is", "define", "translate", "hello", "yes or no", "capital of",
    "how old", "who is", "when was",
    "什么是", "定义", "翻译", "你好", "是否", "首都", "多大", "谁是", "何时",
    "とは", "定義", "翻訳", "こんにちは", "はいかいいえ", "首都", "誰",
    "что такое", "определение", "перевести", "переведи", "привет",
    "да или нет", "столица", "сколько лет", "кто такой", "когда", "объясни",
    "was ist", "definiere", "übersetze", "hallo", "ja oder nein",
    "hauptstadt", "wie alt", "wer ist", "wann", "erkläre",
];
const TECHNICAL_KEYWORDS: &[&str] = &[
    "algorithm", "optimize", "architecture", "distributed", "kubernetes",
    "microservice", "database", "infrastructure",
    "算法", "优化", "架构", "分布式", "微服务", "数据库", "基础设施",
    "アルゴリズム", "最適化", "アーキテクチャ", "分散", "マイクロサービス", "データベース",
    "алгоритм", "оптимизировать", "оптимизаци", "оптимизируй",
    "архитектура", "распределённый", "микросервис", "база данных", "инфраструктура",
    "algorithmus", "optimieren", "architektur", "verteilt",
    "mikroservice", "datenbank", "infrastruktur",
];
const CREATIVE_KEYWORDS: &[&str] = &[
    "story", "poem", "compose", "brainstorm", "creative", "imagine", "write a",
    "故事", "诗", "创作", "头脑风暴", "创意", "想象", "写一个",
    "物語", "詩", "作曲", "ブレインストーム", "創造的", "想像",
    "история", "рассказ", "стихотворение", "сочинить", "сочини",
    "мозговой штурм", "творческий", "представить", "придумай", "напиши",
    "geschichte", "gedicht", "komponieren", "brainstorming",
    "kreativ", "vorstellen", "schreibe", "erzählung",
];
const IMPERATIVE_VERBS: &[&str] = &[
    "build", "create", "implement", "design", "develop", "construct",
    "generate", "deploy", "configure", "set up",
    "构建", "创建", "实现", "设计", "开发", "生成", "部署", "配置", "设置",
    "構築", "作成", "実装", "設計", "開発", "生成", "デプロイ", "設定",
    "построить", "построй", "создать", "создай", "реализовать", "реализуй",
    "спроектировать", "разработать", "разработай",
    "erstellen", "bauen", "implementieren", "entwerfen", "entwickeln",
];
const CONSTRAINT_INDICATORS: &[&str] = &[
    "under", "at most", "at least", "within", "no more than", "o(",
    "maximum", "minimum", "limit", "budget",
    "不超过", "至少", "最多", "在内", "最大", "最小", "限制", "预算",
    "以下", "制限", "予算",
    "не более", "не менее", "как минимум", "в пределах", "максимум",
    "минимум", "ограничение", "бюджет",
    "höchstens", "mindestens", "innerhalb", "nicht mehr als",
];
const OUTPUT_FORMAT_KEYWORDS: &[&str] = &[
    "json", "yaml", "xml", "table", "csv", "markdown", "schema", "format as", "structured",
];
const REFERENCE_KEYWORDS: &[&str] = &[
    "above", "below", "previous", "following", "the docs", "the api",
    "the code", "earlier", "attached",
];
const NEGATION_KEYWORDS: &[&str] = &[
    "don't", "do not", "avoid", "never", "without", "except", "exclude", "no longer",
    "不要", "避免", "从不", "没有", "除了",
    "не делай", "не надо", "нельзя", "избегать", "никогда",
    "nicht", "vermeide", "niemals", "ohne",
];
const DOMAIN_SPECIFIC_KEYWORDS: &[&str] = &[
    "quantum", "fpga", "vlsi", "risc-v", "asic", "photonics", "genomics",
    "proteomics", "topological", "homomorphic", "zero-knowledge", "lattice-based",
    "量子", "光子学", "基因组学", "拓扑", "同态", "零知识",
    "квантовый", "фотоника", "геномика", "топологический",
    "quanten", "photonik", "genomik", "topologisch", "homomorph",
];
const AGENTIC_TASK_KEYWORDS: &[&str] = &[
    "read file", "read the file", "look at", "check the", "open the",
    "edit", "modify", "update the", "change the", "write to", "create file",
    "execute", "deploy", "install", "npm", "pip", "compile",
    "after that", "and also", "once done", "step 1", "step 2",
    "fix", "debug", "until it works", "keep trying", "iterate",
    "make sure", "verify", "confirm",
];

// Dimension weights (sum ≈ 1.0, from FreeRouter upstream)
const W_TOKEN_COUNT:         f64 = 0.04;
const W_CODE:                f64 = 0.12;
const W_REASONING:           f64 = 0.25;
const W_TECHNICAL:           f64 = 0.18;
const W_CREATIVE:            f64 = 0.05;
const W_SIMPLE:              f64 = 0.10;
const W_MULTI_STEP:          f64 = 0.12;
const W_QUESTION_COMPLEXITY: f64 = 0.05;
const W_IMPERATIVE:          f64 = 0.06;
const W_CONSTRAINT:          f64 = 0.04;
const W_OUTPUT_FORMAT:       f64 = 0.03;
const W_REFERENCE:           f64 = 0.02;
const W_NEGATION:            f64 = 0.01;
const W_DOMAIN:              f64 = 0.12;
const W_AGENTIC:             f64 = 0.04;

// Tier boundaries (from FreeRouter)
const B_SIMPLE_MEDIUM:    f64 = 0.00;
const B_MEDIUM_COMPLEX:   f64 = 0.03;
const B_COMPLEX_REASONING: f64 = 0.15;

const CONFIDENCE_STEEPNESS: f64 = 8.0;
const CONFIDENCE_THRESHOLD: f64 = 0.50;

// ─── Mode override detection ──────────────────────────────────────────────────

/// Detect a tier prefix in the prompt and strip it.
///
/// Patterns: `/simple`, `[max]`, `deep mode:` …
pub fn detect_mode_override(prompt: &str) -> Option<ModeOverride> {
    let tier_of = |s: &str| match s {
        "simple" | "basic" | "cheap" => Some(PromptTier::Simple),
        "medium" | "balanced"        => Some(PromptTier::Medium),
        "complex" | "advanced"       => Some(PromptTier::Complex),
        "max" | "reasoning" | "think" | "deep" => Some(PromptTier::Reasoning),
        _ => None,
    };

    // /word …
    if let Some(rest) = prompt.strip_prefix('/') {
        let end = rest.find(' ').unwrap_or(rest.len());
        let word = rest[..end].to_lowercase();
        if let Some(tier) = tier_of(&word) {
            let cleaned = rest[end..].trim().to_string();
            if !cleaned.is_empty() {
                return Some(ModeOverride { tier, cleaned_prompt: cleaned });
            }
        }
    }

    // [word] …
    if prompt.starts_with('[') {
        if let Some(close) = prompt.find(']') {
            let word = prompt[1..close].trim().to_lowercase();
            if let Some(tier) = tier_of(&word) {
                let cleaned = prompt[close + 1..].trim().to_string();
                return Some(ModeOverride { tier, cleaned_prompt: cleaned });
            }
        }
    }

    // "word mode:" or "word mode,"
    let lower = prompt.to_lowercase();
    for word in ["simple","basic","cheap","medium","balanced","complex",
                  "advanced","max","reasoning","think","deep"] {
        for pat in [format!("{word} mode:"), format!("{word} mode,")] {
            if lower.starts_with(&pat) {
                if let Some(tier) = tier_of(word) {
                    let cleaned = prompt[pat.len()..].trim().to_string();
                    return Some(ModeOverride { tier, cleaned_prompt: cleaned });
                }
            }
        }
    }

    None
}

// ─── Main classifier ──────────────────────────────────────────────────────────

/// Classify a prompt into a routing tier.
pub fn classify_prompt(
    prompt: &str,
    system_prompt: Option<&str>,
) -> ClassificationResult {
    let user_tokens = prompt.len() / 4 + 1;

    // Force COMPLEX on very large inputs
    let total_len = system_prompt.map(|s| s.len()).unwrap_or(0) + prompt.len();
    if total_len / 4 > 25_000 {
        return ClassificationResult {
            score: 0.95,
            tier: Some(PromptTier::Complex),
            confidence: 0.95,
            signals: vec![format!("large input (~{} tokens)", total_len / 4)],
            agentic_score: 0.0,
        };
    }

    let combined = match system_prompt {
        Some(s) => format!("{s} {prompt}"),
        None => prompt.to_string(),
    };
    let text = combined.to_lowercase();
    let user = prompt.to_lowercase();

    let has_structured_output = user.contains("json") || user.contains("schema") || user.contains("structured");

    // ── Dimension scoring ────────────────────────────────────────────────────
    let tok = score_token_count(user_tokens);
    let code = score_kw(&text, CODE_KEYWORDS, 1, 2, 0.0, 0.5, 1.0);
    let reasoning = score_kw(&user, REASONING_KEYWORDS, 1, 2, 0.0, 0.7, 1.0);
    let technical = score_kw(&text, TECHNICAL_KEYWORDS, 2, 4, 0.0, 0.5, 1.0);
    let creative = score_kw(&text, CREATIVE_KEYWORDS, 1, 2, 0.0, 0.5, 0.7);
    let simple = score_kw(&text, SIMPLE_KEYWORDS, 1, 2, 0.0, -1.0, -1.0);
    let multi_step = score_multi_step(&text);
    let question = score_question_complexity(prompt);
    let imperative = score_kw(&text, IMPERATIVE_VERBS, 1, 2, 0.0, 0.3, 0.5);
    let constraint = score_kw(&text, CONSTRAINT_INDICATORS, 1, 3, 0.0, 0.3, 0.7);
    let format = score_kw(&text, OUTPUT_FORMAT_KEYWORDS, 1, 2, 0.0, 0.4, 0.7);
    let reference = score_kw(&text, REFERENCE_KEYWORDS, 1, 2, 0.0, 0.3, 0.5);
    let negation = score_kw(&text, NEGATION_KEYWORDS, 2, 3, 0.0, 0.3, 0.5);
    let domain = score_kw(&text, DOMAIN_SPECIFIC_KEYWORDS, 1, 2, 0.0, 0.5, 0.8);
    let (agentic, agentic_score) = score_agentic(&text);

    let dims: &[(f64, &DimResult)] = &[
        (W_TOKEN_COUNT, &tok),
        (W_CODE, &code),
        (W_REASONING, &reasoning),
        (W_TECHNICAL, &technical),
        (W_CREATIVE, &creative),
        (W_SIMPLE, &simple),
        (W_MULTI_STEP, &multi_step),
        (W_QUESTION_COMPLEXITY, &question),
        (W_IMPERATIVE, &imperative),
        (W_CONSTRAINT, &constraint),
        (W_OUTPUT_FORMAT, &format),
        (W_REFERENCE, &reference),
        (W_NEGATION, &negation),
        (W_DOMAIN, &domain),
        (W_AGENTIC, &agentic),
    ];

    let signals: Vec<String> = dims.iter()
        .filter_map(|(_, d)| d.signal.clone())
        .collect();

    let weighted_score: f64 = dims.iter().map(|(w, d)| w * d.score).sum();

    // ── Direct reasoning override: 2+ reasoning markers ─────────────────────
    let reasoning_matches = REASONING_KEYWORDS.iter()
        .filter(|kw| user.contains(**kw))
        .count();
    if reasoning_matches >= 2 {
        let conf = sigmoid(weighted_score.max(0.3), CONFIDENCE_STEEPNESS).max(0.85);
        return ClassificationResult { score: weighted_score, tier: Some(PromptTier::Reasoning), confidence: conf, signals, agentic_score };
    }

    // ── Map score to tier ────────────────────────────────────────────────────
    let (mut tier, dist) = if weighted_score < B_SIMPLE_MEDIUM {
        (PromptTier::Simple,    B_SIMPLE_MEDIUM - weighted_score)
    } else if weighted_score < B_MEDIUM_COMPLEX {
        (PromptTier::Medium,    f64::min(weighted_score - B_SIMPLE_MEDIUM, B_MEDIUM_COMPLEX - weighted_score))
    } else if weighted_score < B_COMPLEX_REASONING {
        (PromptTier::Complex,   f64::min(weighted_score - B_MEDIUM_COMPLEX, B_COMPLEX_REASONING - weighted_score))
    } else {
        (PromptTier::Reasoning, weighted_score - B_COMPLEX_REASONING)
    };

    if has_structured_output && tier == PromptTier::Simple {
        tier = PromptTier::Medium;
    }

    let conf = sigmoid(dist, CONFIDENCE_STEEPNESS);
    if conf < CONFIDENCE_THRESHOLD {
        return ClassificationResult { score: weighted_score, tier: None, confidence: conf, signals, agentic_score };
    }

    ClassificationResult { score: weighted_score, tier: Some(tier), confidence: conf, signals, agentic_score }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

struct DimResult { score: f64, signal: Option<String> }

fn score_token_count(tokens: usize) -> DimResult {
    if tokens < 5 {
        DimResult { score: -1.0, signal: Some(format!("short ({tokens} tokens)")) }
    } else if tokens > 40 {
        DimResult { score: 1.0, signal: Some(format!("long ({tokens} tokens)")) }
    } else {
        DimResult { score: 0.0, signal: None }
    }
}

fn score_kw(text: &str, kws: &[&str], low: usize, high: usize,
             sn: f64, sl: f64, sh: f64) -> DimResult {
    let hits: Vec<&&str> = kws.iter().filter(|k| text.contains(**k)).collect();
    if hits.len() >= high {
        let top: String = hits.iter().take(3).map(|k| **k).collect::<Vec<_>>().join(", ");
        DimResult { score: sh, signal: Some(top) }
    } else if hits.len() >= low {
        let top: String = hits.iter().take(3).map(|k| **k).collect::<Vec<_>>().join(", ");
        DimResult { score: sl, signal: Some(top) }
    } else {
        DimResult { score: sn, signal: None }
    }
}

fn score_multi_step(text: &str) -> DimResult {
    let hit = (text.contains("first") && text.contains("then"))
        || text.contains("step 1") || text.contains("step 2");
    DimResult { score: if hit { 0.5 } else { 0.0 }, signal: if hit { Some("multi-step".into()) } else { None } }
}

fn score_question_complexity(prompt: &str) -> DimResult {
    let count = prompt.chars().filter(|&c| c == '?').count();
    DimResult { score: if count > 3 { 0.5 } else { 0.0 }, signal: if count > 3 { Some(format!("{count} questions")) } else { None } }
}

fn score_agentic(text: &str) -> (DimResult, f64) {
    let mut count = 0usize;
    let mut sigs: Vec<&str> = vec![];
    for kw in AGENTIC_TASK_KEYWORDS {
        if text.contains(kw) { count += 1; if sigs.len() < 3 { sigs.push(kw); } }
    }
    let (score, ag) = if count >= 4 { (1.0, 1.0) } else if count >= 3 { (0.6, 0.6) } else if count >= 1 { (0.2, 0.2) } else { (0.0, 0.0) };
    let sig = if count > 0 { Some(format!("agentic ({})", sigs.join(", "))) } else { None };
    (DimResult { score, signal: sig }, ag)
}

fn sigmoid(x: f64, s: f64) -> f64 { 1.0 / (1.0 + (-s * x).exp()) }

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_prompt_is_simple() {
        let r = classify_prompt("what is the capital of France?", None);
        assert_eq!(r.tier, Some(PromptTier::Simple), "score={:.3}", r.score);
    }

    #[test]
    fn code_prompt_medium_or_complex() {
        let r = classify_prompt("implement a binary search in Rust with tests", None);
        assert!(r.tier == Some(PromptTier::Medium) || r.tier == Some(PromptTier::Complex));
    }

    #[test]
    fn reasoning_prompt_is_reasoning() {
        let r = classify_prompt("prove step by step that sqrt(2) is irrational using mathematical proof", None);
        assert_eq!(r.tier, Some(PromptTier::Reasoning), "score={:.3}", r.score);
    }

    #[test]
    fn large_input_forces_complex() {
        let big = "word ".repeat(25_000 * 4);
        let r = classify_prompt(&big, None);
        assert_eq!(r.tier, Some(PromptTier::Complex));
    }

    #[test]
    fn structured_output_upgrades_simple() {
        let r = classify_prompt("hello, return as json", None);
        assert_ne!(r.tier, Some(PromptTier::Simple));
    }

    #[test]
    fn slash_override() {
        let ov = detect_mode_override("/simple explain this").unwrap();
        assert_eq!(ov.tier, PromptTier::Simple);
        assert_eq!(ov.cleaned_prompt, "explain this");
    }

    #[test]
    fn bracket_override() {
        let ov = detect_mode_override("[max] prove riemann hypothesis").unwrap();
        assert_eq!(ov.tier, PromptTier::Reasoning);
    }

    #[test]
    fn mode_word_override() {
        let ov = detect_mode_override("deep mode: analyse this").unwrap();
        assert_eq!(ov.tier, PromptTier::Reasoning);
        assert_eq!(ov.cleaned_prompt, "analyse this");
    }

    #[test]
    fn no_override_none() {
        assert!(detect_mode_override("write a hello world").is_none());
    }

    #[test]
    fn prompt_tier_as_str() {
        assert_eq!(PromptTier::Simple.as_str(), "SIMPLE");
        assert_eq!(PromptTier::Reasoning.as_str(), "REASONING");
    }
}
