# Custom Formula Derived Data Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build reusable custom formulas that generate derived waveform variables, with autocomplete, explicit channel mapping, plotting, measurement, export, config persistence, docs, and version updates.

**Architecture:** Add pure formula parsing/evaluation and completion modules, then replace the fixed PLL/dq0-only derived path with a generalized derived definition/output model. Keep computed samples transient and route formulas through the existing background job, cancellation, plot preparation, measurement, and export flows.

**Tech Stack:** Rust 2021, existing `eframe`/`egui` UI, existing `DataSource`/`SampleBlock` model, `serde`/`serde_json` configs, existing thread-based job helpers, `cargo test`, `cargo fmt`, `cargo clippy`.

---

## File Structure

- Create `src/formula.rs`: tokenizer, parser, AST, validation, reference extraction, vector evaluation, formula tests.
- Create `src/formula_completion.rs`: function metadata, channel catalog candidates, prefix ranking, completion tests.
- Create `src/derived.rs`: generalized derived definitions, built-in PLL/dq0 metadata, formula definitions, mappings, cache keys, channel resolution tests.
- Modify `src/main.rs`: expose new modules.
- Modify `src/app/state.rs`: add formula UI/config state and replace fixed derived state fields touched by generalized output selection.
- Modify `src/app/plot.rs`: replace `derived: Vec<usize>` with stable derived output ids or indexes that can represent built-in and formula outputs.
- Modify `src/app/jobs.rs`: keep cancellation helpers and route generalized derived worker cancellation through the existing derived worker cancellation references.
- Modify `src/app.rs`: integrate formulas in config menus, channel panel, analysis panel, workers, measurements, plot rendering, export selections, localization, tests.
- Modify `src/app/export.rs`: update export helpers that read derived output names, colors, or selection indexes.
- Modify `Cargo.toml`, `scripts/package-windows.ps1`, `scripts/ScopeAnalyzer.wxs`, `README.md`: bump version and document feature.

Use no new parser dependency in the first implementation. A small Pratt parser is enough for the supported expression language and keeps offline release packaging stable.

---

### Task 1: Add Formula Parser And Evaluator Core

**Files:**
- Create: `src/formula.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Declare the module**

Add this module declaration near the other internal modules in `src/main.rs`:

```rust
mod formula;
```

- [ ] **Step 2: Write parser/evaluator tests first**

Create `src/formula.rs` with the public API and failing tests:

```rust
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub struct Formula {
    source: String,
    ast: Expr,
    references: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Number(f64),
    Reference(String),
    Unary { op: UnaryOp, rhs: Box<Expr> },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Call { name: String, args: Vec<Expr> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormulaError {
    pub position: usize,
    pub message: String,
}

impl FormulaError {
    fn new(position: usize, message: impl Into<String>) -> Self {
        Self {
            position,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FormulaContext<'a> {
    pub times: &'a [f64],
    pub channels: BTreeMap<String, &'a [f32]>,
}

impl Formula {
    pub fn parse(source: &str) -> Result<Self, FormulaError> {
        Err(FormulaError::new(0, format!("parser not implemented for `{source}`")))
    }

    pub fn references(&self) -> &[String] {
        &self.references
    }

    pub fn evaluate(&self, context: &FormulaContext<'_>) -> Result<Vec<f32>, FormulaError> {
        let _ = context;
        Err(FormulaError::new(0, "evaluator not implemented"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(a: &'a [f32], b: &'a [f32]) -> FormulaContext<'a> {
        let mut channels = BTreeMap::new();
        channels.insert("CH1".to_owned(), a);
        channels.insert("CH2".to_owned(), b);
        channels.insert("stVg_0.iA".to_owned(), a);
        channels.insert("stIg_0.iA".to_owned(), b);
        FormulaContext {
            times: &[0.0, 0.1, 0.2],
            channels,
        }
    }

    fn eval(source: &str) -> Vec<f32> {
        let a = [1.0, 2.0, 3.0];
        let b = [10.0, 20.0, 30.0];
        Formula::parse(source).unwrap().evaluate(&context(&a, &b)).unwrap()
    }

    #[test]
    fn arithmetic_precedence_and_parentheses_are_honored() {
        assert_eq!(eval("CH1 + CH2 * 2"), vec![21.0, 42.0, 63.0]);
        assert_eq!(eval("(CH1 + CH2) * 2"), vec![22.0, 44.0, 66.0]);
        assert_eq!(eval("-CH1 + 5"), vec![4.0, 3.0, 2.0]);
        assert_eq!(eval("CH1 ^ 2"), vec![1.0, 4.0, 9.0]);
    }

    #[test]
    fn comparisons_logic_and_if_work_per_sample() {
        assert_eq!(eval("if(CH1 >= 2, CH2, 0)"), vec![0.0, 20.0, 30.0]);
        assert_eq!(eval("if(CH1 > 1 && CH2 < 30, 1, 0)"), vec![0.0, 1.0, 0.0]);
    }

    #[test]
    fn functions_evaluate_vectors_and_window_statistics() {
        assert_eq!(eval("abs(-CH1)"), vec![1.0, 2.0, 3.0]);
        assert_eq!(eval("sqrt(CH1 + 3)"), vec![2.0, 2.236068, 2.4494898]);
        assert_eq!(eval("min(CH1, 2)"), vec![1.0, 2.0, 2.0]);
        assert_eq!(eval("max(CH1, 2)"), vec![2.0, 2.0, 3.0]);
        assert_eq!(eval("clamp(CH2, 15, 25)"), vec![15.0, 20.0, 25.0]);
        assert_eq!(eval("avg(CH1)"), vec![2.0, 2.0, 2.0]);
        let rms = eval("rms(CH1)");
        assert!(rms.iter().all(|value| (*value - 2.1602468).abs() < 1.0e-5));
    }

    #[test]
    fn non_finite_sample_errors_become_nan() {
        let result = eval("CH1 / (CH1 - 2)");
        assert!(result[0].is_finite());
        assert!(result[1].is_nan());
        assert!(result[2].is_finite());

        let result = eval("sqrt(CH1 - 2)");
        assert!(result[0].is_nan());
        assert_eq!(result[1], 0.0);
        assert_eq!(result[2], 1.0);
    }

    #[test]
    fn references_are_extracted_once_in_source_order() {
        let formula = Formula::parse("stVg_0.iA * stIg_0.iA + stVg_0.iA").unwrap();
        assert_eq!(formula.references(), &["stVg_0.iA".to_owned(), "stIg_0.iA".to_owned()]);
    }

    #[test]
    fn invalid_inputs_report_positions() {
        let error = Formula::parse("CH1 + * CH2").unwrap_err();
        assert!(error.position >= 4);
        assert!(error.message.contains("expression"));

        let error = Formula::parse("unknown(CH1)").unwrap_err();
        assert!(error.message.contains("Unknown function"));
    }
}
```

- [ ] **Step 3: Run the new tests to verify they fail**

Run:

```powershell
cargo test formula::tests --lib
```

Expected: compile failure or test failure because `Formula::parse` and `evaluate` are not implemented.

- [ ] **Step 4: Implement tokenizer, Pratt parser, validation, and evaluator**

Replace the failing `Formula::parse` and `Formula::evaluate` methods and add private helpers in `src/formula.rs`:

```rust
impl Formula {
    pub fn parse(source: &str) -> Result<Self, FormulaError> {
        let tokens = tokenize(source)?;
        let mut parser = Parser::new(source, tokens);
        let ast = parser.parse_expression(0)?;
        parser.expect_end()?;
        validate_expr(&ast)?;
        let mut seen = BTreeSet::new();
        let mut references = Vec::new();
        collect_references(&ast, &mut seen, &mut references);
        Ok(Self {
            source: source.to_owned(),
            ast,
            references,
        })
    }

    pub fn evaluate(&self, context: &FormulaContext<'_>) -> Result<Vec<f32>, FormulaError> {
        let len = context.times.len();
        let values = eval_expr(&self.ast, context, len)?;
        Ok(values.into_iter().map(|value| value as f32).collect())
    }
}
```

Implement these private items in the same file:

```rust
#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
    Comma,
    Gt,
    Ge,
    Lt,
    Le,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    Bang,
    End,
}
```

Use these operator precedences:

```rust
fn infix_binding_power(kind: &TokenKind) -> Option<(u8, u8, BinaryOp)> {
    match kind {
        TokenKind::OrOr => Some((1, 2, BinaryOp::Or)),
        TokenKind::AndAnd => Some((3, 4, BinaryOp::And)),
        TokenKind::EqEq => Some((5, 6, BinaryOp::Eq)),
        TokenKind::Ne => Some((5, 6, BinaryOp::Ne)),
        TokenKind::Gt => Some((7, 8, BinaryOp::Gt)),
        TokenKind::Ge => Some((7, 8, BinaryOp::Ge)),
        TokenKind::Lt => Some((7, 8, BinaryOp::Lt)),
        TokenKind::Le => Some((7, 8, BinaryOp::Le)),
        TokenKind::Plus => Some((9, 10, BinaryOp::Add)),
        TokenKind::Minus => Some((9, 10, BinaryOp::Sub)),
        TokenKind::Star => Some((11, 12, BinaryOp::Mul)),
        TokenKind::Slash => Some((11, 12, BinaryOp::Div)),
        TokenKind::Caret => Some((14, 13, BinaryOp::Pow)),
        _ => None,
    }
}
```

Implement function validation with exact arities:

```rust
fn function_arity(name: &str) -> Option<std::ops::RangeInclusive<usize>> {
    match name {
        "abs" | "sqrt" | "sin" | "cos" | "tan" | "avg" | "rms" => Some(1..=1),
        "min" | "max" => Some(2..=2),
        "clamp" | "if" => Some(3..=3),
        _ => None,
    }
}
```

Evaluator rules:

```rust
fn bool_to_f64(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

fn truthy(value: f64) -> bool {
    value.is_finite() && value != 0.0
}
```

For `avg` and `rms`, evaluate the single argument vector first, compute over finite samples only, and repeat the aggregate for `len` output samples. If there are no finite samples, return `NaN` repeated.

- [ ] **Step 5: Run parser/evaluator tests**

Run:

```powershell
cargo test formula::tests --lib
```

Expected: all `formula::tests` pass.

- [ ] **Step 6: Commit**

```powershell
git add src/formula.rs src/main.rs
git commit -m "feat: add formula parser and evaluator"
```

---

### Task 2: Add Formula Completion

**Files:**
- Create: `src/formula_completion.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Declare the module**

Add to `src/main.rs`:

```rust
mod formula_completion;
```

- [ ] **Step 2: Write completion tests first**

Create `src/formula_completion.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Function,
    Channel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub kind: CompletionKind,
    pub label: String,
    pub detail: String,
    pub insert_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelCompletion {
    pub raw_name: String,
    pub display_name: String,
    pub alias: String,
}

pub fn complete_formula(prefix: &str, channels: &[ChannelCompletion]) -> Vec<CompletionCandidate> {
    let _ = (prefix, channels);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels() -> Vec<ChannelCompletion> {
        vec![
            ChannelCompletion {
                raw_name: "stVg_0.iA".to_owned(),
                display_name: "Voltage A".to_owned(),
                alias: "CH1".to_owned(),
            },
            ChannelCompletion {
                raw_name: "stIg_0.iA".to_owned(),
                display_name: "Current A".to_owned(),
                alias: "CH2".to_owned(),
            },
        ]
    }

    #[test]
    fn function_prefix_returns_signature_and_insert_text() {
        let candidates = complete_formula("sq", &channels());
        assert_eq!(candidates[0].label, "sqrt");
        assert_eq!(candidates[0].detail, "sqrt(x)");
        assert_eq!(candidates[0].insert_text, "sqrt()");
    }

    #[test]
    fn one_letter_prefix_can_suggest_rms() {
        let candidates = complete_formula("r", &channels());
        assert!(candidates.iter().any(|candidate| candidate.label == "rms"));
    }

    #[test]
    fn channel_prefix_searches_alias_raw_and_display_names() {
        let by_alias = complete_formula("CH", &channels());
        assert_eq!(by_alias[0].label, "CH1");

        let by_raw = complete_formula("stVg", &channels());
        assert_eq!(by_raw[0].insert_text, "stVg_0.iA");

        let by_display = complete_formula("Volt", &channels());
        assert_eq!(by_display[0].insert_text, "stVg_0.iA");
    }
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```powershell
cargo test formula_completion::tests --lib
```

Expected: failure because `complete_formula` is not implemented.

- [ ] **Step 4: Implement function metadata and prefix ranking**

Add function metadata:

```rust
struct FunctionInfo {
    name: &'static str,
    signature: &'static str,
    description: &'static str,
}

const FUNCTIONS: &[FunctionInfo] = &[
    FunctionInfo { name: "abs", signature: "abs(x)", description: "Absolute value" },
    FunctionInfo { name: "sqrt", signature: "sqrt(x)", description: "Square root" },
    FunctionInfo { name: "sin", signature: "sin(x)", description: "Sine" },
    FunctionInfo { name: "cos", signature: "cos(x)", description: "Cosine" },
    FunctionInfo { name: "tan", signature: "tan(x)", description: "Tangent" },
    FunctionInfo { name: "min", signature: "min(a, b)", description: "Smaller value" },
    FunctionInfo { name: "max", signature: "max(a, b)", description: "Larger value" },
    FunctionInfo { name: "clamp", signature: "clamp(x, min, max)", description: "Clamp value" },
    FunctionInfo { name: "avg", signature: "avg(x)", description: "Window average" },
    FunctionInfo { name: "rms", signature: "rms(x)", description: "Window RMS" },
    FunctionInfo { name: "if", signature: "if(condition, true_value, false_value)", description: "Conditional value" },
];
```

Implement ranking:

```rust
fn match_score(prefix: &str, text: &str) -> Option<u8> {
    let prefix = prefix.to_lowercase();
    let text = text.to_lowercase();
    if prefix.is_empty() {
        return None;
    }
    if text.starts_with(&prefix) {
        Some(0)
    } else if text.contains(&prefix) {
        Some(1)
    } else {
        None
    }
}
```

`complete_formula` should collect function and channel candidates, sort by score then label, and return all matches.

- [ ] **Step 5: Run completion tests**

Run:

```powershell
cargo test formula_completion::tests --lib
```

Expected: all completion tests pass.

- [ ] **Step 6: Commit**

```powershell
git add src/formula_completion.rs src/main.rs
git commit -m "feat: add formula autocomplete candidates"
```

---

### Task 3: Add Generalized Derived Model

**Files:**
- Create: `src/derived.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Declare the module**

Add to `src/main.rs`:

```rust
mod derived;
```

- [ ] **Step 2: Write derived model and mapping tests first**

Create `src/derived.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DerivedKind {
    BuiltInPllDq0,
    Formula(FormulaDefinition),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedDefinition {
    pub id: String,
    pub name: String,
    pub unit: String,
    pub description: String,
    pub kind: DerivedKind,
    pub enabled: bool,
    pub pane: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormulaDefinition {
    pub expression: String,
    pub references: Vec<FormulaReference>,
    pub mappings: Vec<DatasetFormulaMapping>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormulaReference {
    pub token: String,
    pub raw_name: Option<String>,
    pub display_name: Option<String>,
    pub alias: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetFormulaMapping {
    pub dataset_key: String,
    pub token: String,
    pub channel_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub index: usize,
    pub raw_name: String,
    pub display_name: String,
    pub alias: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveStatus {
    Ready(Vec<(String, usize)>),
    NeedsMapping(Vec<String>),
}

pub fn built_in_pll_dq0_definitions() -> Vec<DerivedDefinition> {
    Vec::new()
}

pub fn resolve_formula_references(
    dataset_key: &str,
    formula: &FormulaDefinition,
    channels: &[ChannelIdentity],
) -> ResolveStatus {
    ResolveStatus::NeedsMapping(
        formula
            .references
            .iter()
            .map(|reference| reference.token.clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels() -> Vec<ChannelIdentity> {
        vec![
            ChannelIdentity {
                index: 0,
                raw_name: "stVg_0.iA".to_owned(),
                display_name: "Voltage A".to_owned(),
                alias: "CH1".to_owned(),
            },
            ChannelIdentity {
                index: 1,
                raw_name: "stIg_0.iA".to_owned(),
                display_name: "Current A".to_owned(),
                alias: "CH2".to_owned(),
            },
        ]
    }

    fn formula(token: &str) -> FormulaDefinition {
        FormulaDefinition {
            expression: token.to_owned(),
            references: vec![FormulaReference {
                token: token.to_owned(),
                raw_name: Some(token.to_owned()),
                display_name: None,
                alias: None,
            }],
            mappings: Vec::new(),
            revision: 1,
        }
    }

    #[test]
    fn built_in_pll_dq0_definitions_keep_existing_names() {
        let definitions = built_in_pll_dq0_definitions();
        let names = definitions.iter().map(|definition| definition.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["PLL theta (deg)", "dq0.d", "dq0.q", "dq0.0"]);
    }

    #[test]
    fn resolve_prefers_manual_mapping_then_names_then_aliases() {
        let mut by_mapping = formula("Missing");
        by_mapping.mappings.push(DatasetFormulaMapping {
            dataset_key: "dataset-a".to_owned(),
            token: "Missing".to_owned(),
            channel_index: 1,
        });
        assert_eq!(
            resolve_formula_references("dataset-a", &by_mapping, &channels()),
            ResolveStatus::Ready(vec![("Missing".to_owned(), 1)])
        );

        assert_eq!(
            resolve_formula_references("dataset-a", &formula("stVg_0.iA"), &channels()),
            ResolveStatus::Ready(vec![("stVg_0.iA".to_owned(), 0)])
        );

        let alias_formula = FormulaDefinition {
            expression: "CH2".to_owned(),
            references: vec![FormulaReference {
                token: "CH2".to_owned(),
                raw_name: None,
                display_name: None,
                alias: Some("CH2".to_owned()),
            }],
            mappings: Vec::new(),
            revision: 1,
        };
        assert_eq!(
            resolve_formula_references("dataset-a", &alias_formula, &channels()),
            ResolveStatus::Ready(vec![("CH2".to_owned(), 1)])
        );
    }

    #[test]
    fn unresolved_references_need_mapping() {
        assert_eq!(
            resolve_formula_references("dataset-a", &formula("NoSuchChannel"), &channels()),
            ResolveStatus::NeedsMapping(vec!["NoSuchChannel".to_owned()])
        );
    }
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```powershell
cargo test derived::tests --lib
```

Expected: failure because derived helpers are not implemented.

- [ ] **Step 4: Implement built-ins and resolver**

Implement built-ins:

```rust
pub fn built_in_pll_dq0_definitions() -> Vec<DerivedDefinition> {
    ["PLL theta (deg)", "dq0.d", "dq0.q", "dq0.0"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| DerivedDefinition {
            id: format!("builtin:pll_dq0:{index}"),
            name: name.to_owned(),
            unit: String::new(),
            description: "Built-in PLL/dq0 derived curve".to_owned(),
            kind: DerivedKind::BuiltInPllDq0,
            enabled: false,
            pane: 0,
        })
        .collect()
}
```

Implement `resolve_formula_references` in the documented order:

1. Matching `DatasetFormulaMapping`.
2. Exact raw name.
3. Exact display name.
4. Exact alias.
5. Case-insensitive raw/display name.
6. Needs mapping.

- [ ] **Step 5: Run derived tests**

Run:

```powershell
cargo test derived::tests --lib
```

Expected: all derived tests pass.

- [ ] **Step 6: Commit**

```powershell
git add src/derived.rs src/main.rs
git commit -m "feat: add derived formula model"
```

---

### Task 4: Add Formula Config Section

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/state.rs`

- [ ] **Step 1: Add failing config tests**

In the existing `#[cfg(test)] mod tests` in `src/app.rs`, extend config tests:

```rust
#[test]
fn formula_config_section_defines_default_files_and_labels() {
    assert_eq!(ConfigSection::Formula.default_file_name(), "scope-formulas.json");
    assert_eq!(
        ConfigSection::Formula.recent_file_name(),
        "scope-recent-formula-configs.json"
    );
    assert_eq!(ConfigSection::Formula.label(Language::En), "Formula Settings");
    assert_eq!(ConfigSection::Formula.label(Language::Zh), "公式配置");
}

#[test]
fn formula_config_files_are_tagged_by_section() {
    let json = ScopeApp::serialize_config_file(
        ConfigSection::Formula,
        &FormulaConfig {
            formulas: Vec::new(),
        },
    )
    .unwrap();
    assert!(json.contains(r#""scope_config_type": "formulas""#));
    let decoded: FormulaConfig =
        ScopeApp::deserialize_config_file(ConfigSection::Formula, &json).unwrap();
    assert!(decoded.formulas.is_empty());
}
```

- [ ] **Step 2: Run config tests to verify failure**

Run:

```powershell
cargo test config_ --lib
```

Expected: failure because `ConfigSection::Formula` and `FormulaConfig` do not exist.

- [ ] **Step 3: Add config structures and recent lists**

Add a `Formula` variant to `ConfigSection` and update all `match` expressions:

```rust
enum ConfigSection {
    Names,
    Display,
    Shortcut,
    Dataset,
    Formula,
}
```

Add formula config:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FormulaConfig {
    #[serde(default)]
    formulas: Vec<crate::derived::DerivedDefinition>,
}
```

Add to `ScopeApp` state in `src/app/state.rs`:

```rust
pub(super) formula_definitions: Vec<crate::derived::DerivedDefinition>,
pub(super) recent_formula_configs: Vec<PathBuf>,
pub(super) show_formula_manager: bool,
```

Initialize them in `ScopeApp::new`:

```rust
let recent_formula_configs = Self::load_recent_configs(ConfigSection::Formula);
```

and:

```rust
formula_definitions: crate::derived::built_in_pll_dq0_definitions(),
recent_formula_configs,
show_formula_manager: false,
```

- [ ] **Step 4: Add import/export functions and menu entries**

Add `current_formula_config`, `apply_formula_config`, `export_formula_config`, `import_formula_config`, and `import_formula_config_from_path` following existing display/dataset config patterns.

Add `Config > Formula Settings` to the top menu near other config sections:

```rust
ui.menu_button(self.t(UiText::FormulaConfig), |ui| {
    if ui.button(Self::icon_label("\u{E8B5}", self.t(UiText::ImportAction))).clicked() {
        self.import_formula_config();
        ui.close_menu();
    }
    if ui.button(Self::icon_label("\u{EDE1}", self.t(UiText::ExportAction))).clicked() {
        self.export_formula_config();
        ui.close_menu();
    }
    self.config_recent_menu(ui, ConfigSection::Formula);
});
```

Add `UiText::FormulaConfig` with:

```rust
(FormulaConfig, Language::Zh) => "公式配置",
(FormulaConfig, Language::En) => "Formula Settings",
```

- [ ] **Step 5: Run config tests**

Run:

```powershell
cargo test config_ --lib
```

Expected: config tests pass.

- [ ] **Step 6: Commit**

```powershell
git add src/app.rs src/app/state.rs
git commit -m "feat: add formula settings config section"
```

---

### Task 5: Generalize Derived Selection State Without Changing Behavior

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/state.rs`
- Modify: `src/app/plot.rs`

- [ ] **Step 1: Add regression tests for current PLL/dq0 names and selection**

Keep or add tests in `src/app.rs`:

```rust
#[test]
fn built_in_derived_outputs_keep_existing_order_and_names() {
    let app = ScopeApp::default_for_tests();
    let names = app
        .formula_definitions
        .iter()
        .filter(|definition| matches!(definition.kind, crate::derived::DerivedKind::BuiltInPllDq0))
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["PLL theta (deg)", "dq0.d", "dq0.q", "dq0.0"]);
}
```

When the app has no lightweight constructor, skip this app-level test and rely on the pure `derived::built_in_pll_dq0_definitions` regression from Task 3. Do not create an egui context just for this test.

- [ ] **Step 2: Replace fixed derived arrays with definition-backed state**

Replace direct assumptions around:

- `DERIVED_CHANNEL_COUNT`
- `DERIVED_CHANNEL_NAMES`
- `derived_visible`
- `derived_colors`
- `derived_line_patterns`
- `derived_panes`

with helper methods that read/write `formula_definitions` for built-in outputs:

```rust
fn derived_output_count(&self) -> usize {
    self.formula_definitions.len()
}

fn derived_output_name(&self, index: usize) -> &str {
    self.formula_definitions
        .get(index)
        .map(|definition| definition.name.as_str())
        .unwrap_or("derived")
}

fn selected_derived_outputs(&self) -> Vec<usize> {
    self.formula_definitions
        .iter()
        .enumerate()
        .filter_map(|(index, definition)| definition.enabled.then_some(index))
        .collect()
}
```

Keep old helper names as wrappers during this task to reduce diff size:

```rust
fn derived_channel_name(&self, index: usize) -> &str {
    self.derived_output_name(index)
}
```

- [ ] **Step 3: Update plot selection types**

In `src/app/plot.rs`, keep `derived: Vec<usize>` for this task and document that it indexes generalized derived outputs, not just PLL/dq0:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PlotSelections {
    pub(super) primary: Vec<usize>,
    pub(super) imported: Vec<Vec<usize>>,
    pub(super) derived: Vec<usize>,
}
```

Update comments or names later only if call sites are clear.

- [ ] **Step 4: Run existing derived/plot tests**

Run:

```powershell
cargo test derived --lib
cargo test plot --lib
```

Expected: tests pass and no behavior changes for PLL/dq0.

- [ ] **Step 5: Commit**

```powershell
git add src/app.rs src/app/state.rs src/app/plot.rs
git commit -m "refactor: generalize derived selection state"
```

---

### Task 6: Add Formula Manager UI Shell And Autocomplete Popup

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/state.rs`

- [ ] **Step 1: Add UI state fields**

Add to `ScopeApp`:

```rust
pub(super) formula_editor_name: String,
pub(super) formula_editor_expression: String,
pub(super) formula_editor_unit: String,
pub(super) formula_editor_description: String,
pub(super) formula_editor_selected: Option<usize>,
pub(super) formula_editor_error: Option<String>,
pub(super) formula_completion_prefix: String,
pub(super) formula_completion_candidates: Vec<crate::formula_completion::CompletionCandidate>,
```

Initialize with empty strings/vectors.

- [ ] **Step 2: Add localized labels**

Add `UiText` entries:

```rust
FormulaManager,
FormulaName,
FormulaExpression,
FormulaUnit,
FormulaDescription,
Validate,
Save,
ApplyToSelectedDatasets,
FormulaNeedsMapping,
FormulaInvalid,
FormulaReady,
```

Add Chinese/English strings:

```rust
(FormulaManager, Language::Zh) => "公式管理",
(FormulaManager, Language::En) => "Formula Manager",
(FormulaName, Language::Zh) => "结果变量名",
(FormulaName, Language::En) => "Result Name",
(FormulaExpression, Language::Zh) => "公式",
(FormulaExpression, Language::En) => "Expression",
```

- [ ] **Step 3: Add formula manager window**

Add:

```rust
fn formula_manager_window(&mut self, ctx: &egui::Context) {
    if !self.show_formula_manager {
        return;
    }
    let mut open = self.show_formula_manager;
    egui::Window::new(self.t(UiText::FormulaManager))
        .open(&mut open)
        .resizable(true)
        .default_width(720.0)
        .show(ctx, |ui| {
            self.formula_manager_ui(ui);
        });
    self.show_formula_manager = open;
}
```

Call `self.formula_manager_window(ctx);` from the main app update path alongside other windows.

Implement `formula_manager_ui` with:

- left list of non-built-in formula definitions;
- text fields for name/expression/unit/description;
- validation status;
- `Validate`, `Save`, `Apply to selected datasets`, `Delete` buttons.

- [ ] **Step 4: Wire autocomplete candidates**

Create:

```rust
fn formula_completion_channels(&self) -> Vec<crate::formula_completion::ChannelCompletion> {
    self.meta()
        .map(|meta| {
            meta.channels
                .iter()
                .map(|channel| crate::formula_completion::ChannelCompletion {
                    raw_name: channel.name.clone(),
                    display_name: self
                        .display_names
                        .get(channel.index)
                        .cloned()
                        .unwrap_or_else(|| channel.name.clone()),
                    alias: format!("CH{}", channel.index + 1),
                })
                .collect()
        })
        .unwrap_or_default()
}
```

When the expression field changes, compute the current token prefix with a small helper:

```rust
fn formula_completion_prefix_at_end(text: &str) -> String {
    text.chars()
        .rev()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '.')
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}
```

If prefix length is at least 1, call `complete_formula`.

- [ ] **Step 5: Add a top-level entry point**

Add an `Analysis` or `Config` button:

```rust
if ui.button(self.t(UiText::FormulaManager)).clicked() {
    self.show_formula_manager = true;
}
```

Place it near the right-side `Analysis` controls first. If it crowds the panel, move it to top `Config`.

- [ ] **Step 6: Run UI compile check**

Run:

```powershell
cargo check
```

Expected: build succeeds.

- [ ] **Step 7: Commit**

```powershell
git add src/app.rs src/app/state.rs
git commit -m "feat: add formula manager ui shell"
```

---

### Task 7: Save Formulas And Resolve Channel Mappings

**Files:**
- Modify: `src/app.rs`
- Modify: `src/derived.rs`

- [ ] **Step 1: Add save validation tests**

Add tests in `src/derived.rs` or `src/app.rs` for creating `FormulaDefinition` from parser references:

```rust
#[test]
fn formula_definition_records_parser_references() {
    let parsed = crate::formula::Formula::parse("CH1 * stIg_0.iA").unwrap();
    let definition = FormulaDefinition::from_parsed("Power", "W", "", "CH1 * stIg_0.iA", &parsed, 1);
    let tokens = definition.references.iter().map(|reference| reference.token.as_str()).collect::<Vec<_>>();
    assert_eq!(tokens, vec!["CH1", "stIg_0.iA"]);
}
```

- [ ] **Step 2: Implement `FormulaDefinition::from_parsed`**

Add:

```rust
impl FormulaDefinition {
    pub fn from_parsed(
        _name: &str,
        _unit: &str,
        _description: &str,
        expression: &str,
        parsed: &crate::formula::Formula,
        revision: u64,
    ) -> Self {
        Self {
            expression: expression.to_owned(),
            references: parsed
                .references()
                .iter()
                .map(|token| FormulaReference {
                    token: token.clone(),
                    raw_name: Some(token.clone()),
                    display_name: None,
                    alias: token
                        .strip_prefix("CH")
                        .and_then(|suffix| suffix.parse::<usize>().ok())
                        .map(|_| token.clone()),
                })
                .collect(),
            mappings: Vec::new(),
            revision,
        }
    }
}
```

- [ ] **Step 3: Implement Save in Formula Manager**

When `Save` is clicked:

1. Parse `formula_editor_expression`.
2. Reject empty result name.
3. Build `DerivedDefinition { kind: DerivedKind::Formula(...) }`.
4. If editing an existing formula, increment revision.
5. If adding a new formula, assign id `formula:<sanitized-name>:<counter>`.
6. Set `enabled = true` and `pane = current_scope_pane()`.
7. Clear derived caches and mark derived reload needed.

Use this helper:

```rust
fn sanitize_formula_id(name: &str) -> String {
    let mut out = name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_owned()
}
```

- [ ] **Step 4: Add formula row status**

Add:

```rust
fn formula_status_text(&self, definition: &crate::derived::DerivedDefinition) -> &'static str {
    match &definition.kind {
        crate::derived::DerivedKind::BuiltInPllDq0 => self.t(UiText::FormulaReady),
        crate::derived::DerivedKind::Formula(formula) => {
            let channels = self.channel_identities_for_dataset(self.selected_fft_dataset_index());
            match crate::derived::resolve_formula_references(
                &self.dataset_key(self.selected_fft_dataset_index()),
                formula,
                &channels,
            ) {
                crate::derived::ResolveStatus::Ready(_) => self.t(UiText::FormulaReady),
                crate::derived::ResolveStatus::NeedsMapping(_) => self.t(UiText::FormulaNeedsMapping),
            }
        }
    }
}
```

- [ ] **Step 5: Run tests and check**

Run:

```powershell
cargo test formula_definition --lib
cargo check
```

Expected: tests and check pass.

- [ ] **Step 6: Commit**

```powershell
git add src/app.rs src/derived.rs
git commit -m "feat: save formula definitions"
```

---

### Task 8: Evaluate Formula Outputs In Derived Worker

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/state.rs`
- Modify: `src/derived.rs`

- [ ] **Step 1: Split derived calculation by kind**

Replace the current assumption that derived output indexes are exactly the four PLL/dq0 outputs. Add a worker result struct that can hold all selected derived outputs in selection order:

```rust
#[derive(Clone, Debug)]
struct DerivedOutputBlock {
    output_indices: Vec<usize>,
    block: SampleBlock,
}
```

Change `DerivedCurveJobResult.result` to:

```rust
result: Result<DerivedOutputBlock, String>,
```

- [ ] **Step 2: Preserve existing PLL/dq0 calculation**

Rename existing `load_derived_data_with_cancel` to:

```rust
fn load_pll_dq0_data_with_cancel(...)
```

Keep its body unchanged except for the function name. Add a small wrapper that extracts selected built-in indexes from the returned four-channel block.

- [ ] **Step 3: Add formula calculation helper**

Add:

```rust
fn load_formula_data_with_cancel(
    source: Arc<dyn DataSource>,
    start_time: f64,
    end_time: f64,
    definition: &crate::derived::DerivedDefinition,
    resolved: &[(String, usize)],
    channel_scales: &[(String, f32)],
    max_points: usize,
    cancel: Option<&DataCancelToken>,
) -> Result<SampleBlock, String> {
    let crate::derived::DerivedKind::Formula(formula_definition) = &definition.kind else {
        return Err("Derived output is not a formula.".to_owned());
    };
    let source_channels = resolved.iter().map(|(_, channel)| *channel).collect::<Vec<_>>();
    let block = if let Some(cancel) = cancel {
        source.read_range_cancellable(start_time, end_time, &source_channels, max_points, cancel)
    } else {
        source.read_range(start_time, end_time, &source_channels, max_points)
    }
    .map_err(|error| error.to_string())?;
    let parsed = crate::formula::Formula::parse(&formula_definition.expression)
        .map_err(|error| format!("Formula parse error at {}: {}", error.position, error.message))?;
    let mut scaled_buffers: Vec<(String, Vec<f32>)> = Vec::new();
    for (out_index, (token, _)) in resolved.iter().enumerate() {
        let Some(values) = block.channels.get(out_index) else {
            continue;
        };
        let scale = channel_scales
            .iter()
            .find_map(|(scale_token, scale)| (scale_token == token).then_some(*scale))
            .unwrap_or(DEFAULT_CHANNEL_SCALE);
        let scaled = if (scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
            values.clone()
        } else {
            values.iter().map(|value| *value * scale).collect()
        };
        scaled_buffers.push((token.clone(), scaled));
    }
    let channels = scaled_buffers
        .iter()
        .map(|(token, values)| (token.clone(), values.as_slice()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let context = crate::formula::FormulaContext {
        times: &block.times,
        channels,
    };
    let values = parsed.evaluate(&context).map_err(|error| error.message)?;
    Ok(SampleBlock {
        times: block.times,
        channels: vec![values],
    })
}
```

The owned buffers keep scaled channel slices alive for the full formula evaluation call:

```rust
let mut scaled_buffers: Vec<(String, Vec<f32>)> = Vec::new();
for (out_index, (token, _)) in resolved.iter().enumerate() {
    let Some(values) = block.channels.get(out_index) else {
        continue;
    };
    let scale = channel_scales
        .iter()
        .find_map(|(scale_token, scale)| (scale_token == token).then_some(*scale))
        .unwrap_or(DEFAULT_CHANNEL_SCALE);
    let scaled = if (scale - DEFAULT_CHANNEL_SCALE).abs() <= f32::EPSILON {
        values.clone()
    } else {
        values.iter().map(|value| *value * scale).collect()
    };
    scaled_buffers.push((token.clone(), scaled));
}
let channels = scaled_buffers
    .iter()
    .map(|(token, values)| (token.clone(), values.as_slice()))
    .collect::<std::collections::BTreeMap<_, _>>();
```

- [ ] **Step 4: Combine selected outputs**

In the derived worker:

1. Group selected built-in PLL/dq0 outputs.
2. Run PLL/dq0 once when any built-in output is selected.
3. Run each selected formula definition independently.
4. Append output channels to one `SampleBlock`.
5. Store output indexes in the same order as channels.

- [ ] **Step 5: Update prepare path**

Update `prepare_derived_sample_series` so it maps `DerivedOutputBlock.output_indices` to block channel positions rather than assuming `block.channels[derived_index]`.

- [ ] **Step 6: Run PLL/dq0 and formula tests**

Run:

```powershell
cargo test transforms::tests --lib
cargo test formula::tests --lib
cargo check
```

Expected: tests and check pass.

- [ ] **Step 7: Commit**

```powershell
git add src/app.rs src/app/state.rs src/derived.rs
git commit -m "feat: evaluate formula derived outputs"
```

---

### Task 9: Integrate Formula Outputs Into Channel Panel, Plot, Measurement, And Export

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/plot.rs`
- Modify: `src/app/export.rs`

- [ ] **Step 1: Channel panel**

Replace the fixed derived section label with two subsections:

```rust
ui.strong(self.t(UiText::BuiltInDerived));
for index in self.built_in_derived_output_indices() {
    self.derived_output_row_ui(ui, index);
}
ui.strong(self.t(UiText::Formulas));
for index in self.formula_output_indices() {
    self.derived_output_row_ui(ui, index);
}
```

Add labels:

```rust
(BuiltInDerived, Language::Zh) => "内置派生量",
(BuiltInDerived, Language::En) => "Built-in Derived",
(Formulas, Language::Zh) => "公式",
(Formulas, Language::En) => "Formulas",
```

- [ ] **Step 2: Plot rendering**

Update plot rendering loop from:

```rust
for (out_index, derived_index) in &selections.derived {
```

to use generalized names/colors:

```rust
let line_color = self.derived_output_color(*derived_index);
let name = self.derived_output_name(*derived_index).to_owned();
```

Ensure formula outputs can be assigned to panes with the same `derived_scope_pane` logic.

- [ ] **Step 3: Measurement**

Update derived measurement rows to display formula names and colors:

```rust
let color = self.derived_output_color(*derived_index);
let channel_name = self.derived_output_name(*derived_index).to_owned();
```

Ensure formula-derived channels are included in `selected_derived_outputs`.

- [ ] **Step 4: Export preview and output**

Find export curve collection around existing derived export paths and replace fixed names/colors with generalized helpers:

```rust
name: self.derived_output_name(*derived_index).to_owned(),
color: self.derived_output_color(*derived_index),
```

Verify export label defaults use formula result names.

- [ ] **Step 5: Run targeted tests**

Run:

```powershell
cargo test selected_export --lib
cargo test measurement --lib
cargo check
```

Expected: tests and check pass.

- [ ] **Step 6: Commit**

```powershell
git add src/app.rs src/app/plot.rs src/app/export.rs
git commit -m "feat: show formula outputs across plot and export"
```

---

### Task 10: Add Mapping Prompt For Reuse

**Files:**
- Modify: `src/app.rs`
- Modify: `src/app/state.rs`
- Modify: `src/derived.rs`

- [ ] **Step 1: Add mapping UI state**

Add:

```rust
pub(super) show_formula_mapping: bool,
pub(super) formula_mapping_definition_index: Option<usize>,
pub(super) formula_mapping_dataset_index: usize,
pub(super) formula_mapping_missing_tokens: Vec<String>,
pub(super) formula_mapping_choices: Vec<Option<usize>>,
```

- [ ] **Step 2: Trigger mapping from Apply**

When `Apply to selected datasets` is clicked:

1. Resolve the formula for each selected dataset.
2. Enable formula where ready.
3. For the first dataset that needs mapping, fill mapping UI state and open dialog.
4. Summarize any additional datasets that still need mapping after the first is completed.

- [ ] **Step 3: Mapping window**

Add:

```rust
fn formula_mapping_window(&mut self, ctx: &egui::Context) {
    if !self.show_formula_mapping {
        return;
    }
    let mut open = self.show_formula_mapping;
    egui::Window::new(self.t(UiText::FormulaNeedsMapping))
        .open(&mut open)
        .resizable(true)
        .show(ctx, |ui| {
            self.formula_mapping_ui(ui);
        });
    self.show_formula_mapping = open;
}
```

`formula_mapping_ui` should show one row per missing token:

- token label;
- combo box of target dataset channels;
- Apply button;
- Cancel button.

- [ ] **Step 4: Persist mappings**

On Apply, push or replace `DatasetFormulaMapping` entries in the selected formula definition:

```rust
formula.mappings.retain(|mapping| {
    !(mapping.dataset_key == dataset_key && mapping.token == token)
});
formula.mappings.push(DatasetFormulaMapping {
    dataset_key: dataset_key.clone(),
    token: token.clone(),
    channel_index,
});
formula.revision = formula.revision.wrapping_add(1);
```

Clear derived caches after applying mappings.

- [ ] **Step 5: Run check**

Run:

```powershell
cargo check
```

Expected: build succeeds.

- [ ] **Step 6: Commit**

```powershell
git add src/app.rs src/app/state.rs src/derived.rs
git commit -m "feat: map formula channels across datasets"
```

---

### Task 11: Documentation And Version Bump

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `scripts/package-windows.ps1`
- Modify: `scripts/ScopeAnalyzer.wxs`
- Modify: `README.md`
- Modify: `src/app.rs`

- [ ] **Step 1: Bump version to 0.7.0**

Update:

```toml
version = "0.7.0"
```

in `Cargo.toml`.

Update `Cargo.lock` package version for `scope_analyzer` to `0.7.0`.

Update PowerShell package version:

```powershell
$version = "0.7.0"
```

Update WiX Product Version:

```xml
Version="0.7.0"
```

Update README artifact names:

```markdown
- `dist/ScopeAnalyzer-0.7.0-win-x64.zip`
- `dist/ScopeAnalyzer-0.7.0-win-x64.msi`
```

- [ ] **Step 2: Update README formula section**

Add a `Custom Formulas` section:

```markdown
## Custom Formulas

Formula Manager creates reusable derived variables from loaded waveform channels.
Formula results appear under `Formulas` in the left channel list and can be plotted,
measured, and exported like other variables.

Supported operators include `+ - * / ^`, comparisons, logic, parentheses,
`if(condition, a, b)`, and functions such as `abs`, `sqrt`, `sin`, `cos`,
`tan`, `min`, `max`, `clamp`, `avg`, and `rms`.

The formula editor suggests function names and channel names after short prefixes.
When applying a formula to another dataset, Scope Analyzer matches raw names,
display names, and `CHn` aliases. If a channel cannot be matched, the app asks
for an explicit mapping instead of guessing.

Formula definitions can be imported and exported from `Config > Formula Settings`
as `scope-formulas.json`. Computed samples are recalculated from source data and
are not stored in the config.
```

- [ ] **Step 3: Update Help text**

In the in-app Help section in `src/app.rs`, add formula usage text near analysis documentation:

```rust
ui.heading("Custom Formulas");
ui.label("Formula Manager creates derived variables from channel expressions. Formula results appear in the left Formulas group and can be plotted, measured, and exported.");
ui.label("Use + - * / ^, comparisons, logic, if(condition, a, b), and functions such as abs, sqrt, sin, cos, tan, min, max, clamp, avg, and rms.");
ui.label("The editor suggests functions and channel names while typing. Applying a formula to another dataset matches names first and asks for mapping when a channel is missing.");
```

Use localized strings if the surrounding help block is localized; otherwise match the existing help style.

- [ ] **Step 4: Run version sync/preflight checks that are safe locally**

Run:

```powershell
cargo test config_sections_define_default_files_and_labels --lib
cargo test formula --lib
cargo test derived --lib
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add Cargo.toml Cargo.lock scripts/package-windows.ps1 scripts/ScopeAnalyzer.wxs README.md src/app.rs
git commit -m "docs: document formula feature and bump version"
```

---

### Task 12: Final Verification

**Files:**
- Modify only files required by verification fixes.

- [ ] **Step 1: Run full tests**

Run:

```powershell
cargo test
```

Expected: all tests pass.

- [ ] **Step 2: Run formatting**

Run:

```powershell
cargo fmt --check
```

Expected: no formatting changes needed.

- [ ] **Step 3: Run clippy**

Run:

```powershell
cargo clippy --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 4: Run release preflight and record any environment failure**

Run from repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release-check.ps1
```

Expected: preflight passes. When Mesa/ANGLE offline caches or another machine-specific release prerequisite are missing, copy the exact failure into the final handoff and do not claim release readiness.

- [ ] **Step 5: Manual smoke test**

Run:

```powershell
cargo run --release
```

Manual steps:

1. Import a normal CSV dataset.
2. Open Formula Manager.
3. Type `sq` and confirm `sqrt()` appears in autocomplete.
4. Type `CH` and confirm channel aliases appear.
5. Create `P_phaseA = CH1 * CH2`.
6. Check the formula result under `Formulas`.
7. Confirm the curve plots.
8. Measure it between cursors.
9. Export a PNG and confirm the formula curve appears.
10. Export a Word report and confirm the formula curve appears when selected.
11. Import or select a second dataset with mismatched names and confirm mapping prompt appears.

- [ ] **Step 6: Inspect git diff**

Run:

```powershell
git status --short
git diff --stat HEAD
```

Expected: only intentional formula feature files are modified; no `.superpowers/brainstorm` files are staged.

- [ ] **Step 7: Final commit if verification fixes were needed**

When final fixes were required:

```powershell
git diff --name-only HEAD
git add src\app.rs src\app\state.rs src\app\plot.rs src\app\export.rs src\formula.rs src\formula_completion.rs src\derived.rs README.md Cargo.toml Cargo.lock scripts\package-windows.ps1 scripts\ScopeAnalyzer.wxs
git commit -m "fix: stabilize formula verification"
```

Before running `git add`, compare the command's file list with `git diff --name-only HEAD` and remove files that were not changed by the verification fix. Do not stage `.superpowers/brainstorm` or unrelated pre-existing user changes.

When no fixes were required, do not create an empty commit.
