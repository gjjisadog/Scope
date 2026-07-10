use std::fmt;

use serde::{Deserialize, Serialize};

const DIVISION_EPSILON: f64 = 1.0e-12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepidFunction {
    PseqP,
    PseqIp,
    PseqQ,
    PseqIq,
    PseqLl,
    PseqPh,
    Rms3,
    Avg,
    Abs,
}

impl RepidFunction {
    fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "pseqp" => Some(Self::PseqP),
            "pseqip" => Some(Self::PseqIp),
            "pseqq" => Some(Self::PseqQ),
            "pseqiq" => Some(Self::PseqIq),
            "pseqll" => Some(Self::PseqLl),
            "pseqph" => Some(Self::PseqPh),
            "rms3" => Some(Self::Rms3),
            "avg" => Some(Self::Avg),
            "abs" => Some(Self::Abs),
            _ => None,
        }
    }

    fn expected_arg_count(self) -> usize {
        match self {
            Self::PseqP | Self::PseqIp | Self::PseqQ | Self::PseqIq => 6,
            Self::PseqLl | Self::PseqPh | Self::Rms3 | Self::Avg => 3,
            Self::Abs => 1,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::PseqP => "pseqp",
            Self::PseqIp => "pseqip",
            Self::PseqQ => "pseqq",
            Self::PseqIq => "pseqiq",
            Self::PseqLl => "pseqll",
            Self::PseqPh => "pseqph",
            Self::Rms3 => "rms3",
            Self::Avg => "avg",
            Self::Abs => "abs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepidExpressionErrorKind {
    Syntax,
    UnknownFunction,
    WrongArgumentCount,
    InvalidChannel,
    ChannelOutOfRange,
    InputLengthMismatch,
    DivisionByZero,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepidExpressionError {
    kind: RepidExpressionErrorKind,
    message: String,
}

impl RepidExpressionError {
    fn new(kind: RepidExpressionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RepidExpressionErrorKind {
        self.kind
    }
}

impl fmt::Display for RepidExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RepidExpressionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum UnaryOp {
    Negate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum RepidAst {
    Constant(f64),
    Channel(usize),
    Unary {
        op: UnaryOp,
        value: Box<RepidAst>,
    },
    Binary {
        op: BinaryOp,
        left: Box<RepidAst>,
        right: Box<RepidAst>,
    },
    Function {
        function: RepidFunction,
        args: Vec<RepidAst>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepidExpression {
    script: String,
    ast: RepidAst,
    input_channels: Vec<usize>,
}

impl RepidExpression {
    pub fn parse(script: &str) -> Result<Self, RepidExpressionError> {
        let mut parser = Parser::new(script)?;
        let ast = parser.parse_expression()?;
        parser.expect_end()?;
        let mut input_channels = Vec::new();
        collect_input_channels(&ast, &mut input_channels);
        Ok(Self {
            script: script.trim().to_owned(),
            ast,
            input_channels,
        })
    }

    pub fn input_channels(&self) -> &[usize] {
        &self.input_channels
    }

    pub fn signature(&self) -> RepidExpressionSignature {
        RepidExpressionSignature {
            script: self.script.clone(),
            input_channels: self.input_channels.clone(),
            ast_hash: stable_hash_expression(&self.ast),
        }
    }

    pub fn validate_channel_count(&self, channel_count: usize) -> Result<(), RepidExpressionError> {
        if let Some(channel) = self
            .input_channels
            .iter()
            .copied()
            .find(|channel| *channel >= channel_count)
        {
            return Err(RepidExpressionError::new(
                RepidExpressionErrorKind::ChannelOutOfRange,
                format!(
                    "Channel Ch{} does not exist in the current data source.",
                    channel + 1
                ),
            ));
        }
        Ok(())
    }

    pub fn evaluate(&self, inputs: &[&[f32]]) -> Result<Vec<f32>, RepidExpressionError> {
        if inputs.len() < self.input_channels.len() {
            return Err(RepidExpressionError::new(
                RepidExpressionErrorKind::ChannelOutOfRange,
                "Current data source does not provide all channels required by this expression.",
            ));
        }
        let Some(len) = inputs.first().map(|values| values.len()) else {
            return Ok(Vec::new());
        };
        if inputs.iter().any(|values| values.len() != len) {
            return Err(RepidExpressionError::new(
                RepidExpressionErrorKind::InputLengthMismatch,
                "Input channel lengths are inconsistent for this derived curve.",
            ));
        }
        let mut output = Vec::with_capacity(len);
        for sample_index in 0..len {
            let value = evaluate_ast(&self.ast, sample_index, inputs, &self.input_channels)?;
            output.push(value as f32);
        }
        Ok(output)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepidExpressionSignature {
    pub script: String,
    pub input_channels: Vec<usize>,
    pub ast_hash: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepidDerivedCurve {
    pub name: String,
    pub raw_name: String,
    pub script_vars: Vec<(usize, String)>,
    pub script: String,
    #[serde(default)]
    pub min: Option<f32>,
    #[serde(default)]
    pub max: Option<f32>,
    #[serde(default = "default_gain")]
    pub k: f32,
    #[serde(default)]
    pub b: f32,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub pen_style: Option<String>,
    #[serde(default)]
    pub auto_color: bool,
    #[serde(default)]
    pub time_offset: f64,
    #[serde(default = "default_time_scale")]
    pub time_scale: f64,
}

impl RepidDerivedCurve {
    pub fn expression(&self) -> Result<RepidExpression, RepidExpressionError> {
        RepidExpression::parse(&self.script)
    }

    pub fn cache_signature(&self) -> RepidCurveSignature {
        let expression_signature = self
            .expression()
            .map(|expression| expression.signature())
            .unwrap_or_else(|error| RepidExpressionSignature {
                script: self.script.clone(),
                input_channels: Vec::new(),
                ast_hash: stable_hash_text(&format!("error:{:?}:{}", error.kind(), error)),
            });
        RepidCurveSignature {
            name: self.name.clone(),
            expression: expression_signature,
            k_bits: self.k.to_bits(),
            b_bits: self.b.to_bits(),
        }
    }

    pub fn diagnostic(&self, channel_count: Option<usize>) -> String {
        let expression = match self.expression() {
            Ok(expression) => expression,
            Err(error) => return error.to_string(),
        };
        if let Some(channel_count) = channel_count {
            if let Err(error) = expression.validate_channel_count(channel_count) {
                return error.to_string();
            }
        }
        "OK".to_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepidCurveSignature {
    pub name: String,
    pub expression: RepidExpressionSignature,
    pub k_bits: u32,
    pub b_bits: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RepidPresetBases {
    pub base_voltage: f64,
    pub base_mva: f64,
    pub base_current: f64,
}

impl Default for RepidPresetBases {
    fn default() -> Self {
        Self {
            base_voltage: 220.0,
            base_mva: 240.0,
            base_current: 3.95897,
        }
    }
}

pub fn positive_sequence_presets(
    voltage_channels: [usize; 3],
    current_channels: [usize; 3],
    bases: RepidPresetBases,
) -> Vec<RepidDerivedCurve> {
    let va = format!("Ch{}", voltage_channels[0] + 1);
    let vb = format!("Ch{}", voltage_channels[1] + 1);
    let vc = format!("Ch{}", voltage_channels[2] + 1);
    let ia = format!("Ch{}", current_channels[0] + 1);
    let ib = format!("Ch{}", current_channels[1] + 1);
    let ic = format!("Ch{}", current_channels[2] + 1);
    let six = format!("{va},{vb},{vc},{ia},{ib},{ic}");
    [
        (
            "正序电压",
            format!("pseqll({va},{vb},{vc}) / {}", bases.base_voltage),
        ),
        ("正序有功", format!("pseqp({six}) / {}", bases.base_mva)),
        ("正序无功", format!("pseqq({six}) / {}", bases.base_mva)),
        (
            "有功电流",
            format!("pseqip({six}) / {}", bases.base_current),
        ),
        (
            "无功电流",
            format!("pseqiq({six}) / {}", bases.base_current),
        ),
    ]
    .into_iter()
    .map(|(name, script)| RepidDerivedCurve {
        name: name.to_owned(),
        raw_name: name.to_owned(),
        script_vars: Vec::new(),
        script,
        min: None,
        max: None,
        k: 1.0,
        b: 0.0,
        color: None,
        pen_style: None,
        auto_color: true,
        time_offset: 0.0,
        time_scale: 1.0,
    })
    .collect()
}

fn default_gain() -> f32 {
    1.0
}

fn default_time_scale() -> f64 {
    1.0
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Channel(usize),
    Identifier(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
    End,
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(script: &str) -> Result<Self, RepidExpressionError> {
        Ok(Self {
            tokens: tokenize(script)?,
            index: 0,
        })
    }

    fn parse_expression(&mut self) -> Result<RepidAst, RepidExpressionError> {
        self.parse_add_sub()
    }

    fn expect_end(&self) -> Result<(), RepidExpressionError> {
        match self.peek() {
            Token::End => Ok(()),
            token => Err(RepidExpressionError::new(
                RepidExpressionErrorKind::Syntax,
                format!("Unexpected token {token:?} at the end of the expression."),
            )),
        }
    }

    fn parse_add_sub(&mut self) -> Result<RepidAst, RepidExpressionError> {
        let mut left = self.parse_mul_div()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinaryOp::Add,
                Token::Minus => BinaryOp::Subtract,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul_div()?;
            left = RepidAst::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul_div(&mut self) -> Result<RepidAst, RepidExpressionError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinaryOp::Multiply,
                Token::Slash => BinaryOp::Divide,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = RepidAst::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<RepidAst, RepidExpressionError> {
        match self.peek() {
            Token::Plus => {
                self.advance();
                self.parse_unary()
            }
            Token::Minus => {
                self.advance();
                Ok(RepidAst::Unary {
                    op: UnaryOp::Negate,
                    value: Box::new(self.parse_unary()?),
                })
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<RepidAst, RepidExpressionError> {
        match self.advance().clone() {
            Token::Number(value) => Ok(RepidAst::Constant(value)),
            Token::Channel(channel) => Ok(RepidAst::Channel(channel)),
            Token::Identifier(name) => self.parse_function_call(name),
            Token::LParen => {
                let expression = self.parse_expression()?;
                self.expect_rparen()?;
                Ok(expression)
            }
            token => Err(RepidExpressionError::new(
                RepidExpressionErrorKind::Syntax,
                format!("Expected a number, channel, function, or '(' but found {token:?}."),
            )),
        }
    }

    fn parse_function_call(&mut self, name: String) -> Result<RepidAst, RepidExpressionError> {
        let function = RepidFunction::parse(&name).ok_or_else(|| {
            RepidExpressionError::new(
                RepidExpressionErrorKind::UnknownFunction,
                format!("Unknown function '{name}'. Only whitelisted derived-curve functions are supported."),
            )
        })?;
        self.expect_lparen()?;
        let mut args = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            loop {
                args.push(self.parse_expression()?);
                if matches!(self.peek(), Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect_rparen()?;
        let expected = function.expected_arg_count();
        if args.len() != expected {
            return Err(RepidExpressionError::new(
                RepidExpressionErrorKind::WrongArgumentCount,
                format!(
                    "{} expects {expected} arguments, got {}.",
                    function.label(),
                    args.len()
                ),
            ));
        }
        Ok(RepidAst::Function { function, args })
    }

    fn expect_lparen(&mut self) -> Result<(), RepidExpressionError> {
        match self.advance() {
            Token::LParen => Ok(()),
            token => Err(RepidExpressionError::new(
                RepidExpressionErrorKind::Syntax,
                format!("Expected '(' after function name, found {token:?}."),
            )),
        }
    }

    fn expect_rparen(&mut self) -> Result<(), RepidExpressionError> {
        match self.advance() {
            Token::RParen => Ok(()),
            token => Err(RepidExpressionError::new(
                RepidExpressionErrorKind::Syntax,
                format!("Expected ')' but found {token:?}."),
            )),
        }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.index).unwrap_or(&Token::End)
    }

    fn advance(&mut self) -> &Token {
        let index = self.index.min(self.tokens.len().saturating_sub(1));
        self.index = (self.index + 1).min(self.tokens.len());
        &self.tokens[index]
    }
}

fn tokenize(script: &str) -> Result<Vec<Token>, RepidExpressionError> {
    let chars = script.trim().chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        match ch {
            '+' => tokens.push(Token::Plus),
            '-' => tokens.push(Token::Minus),
            '*' => tokens.push(Token::Star),
            '/' => tokens.push(Token::Slash),
            '(' => tokens.push(Token::LParen),
            ')' => tokens.push(Token::RParen),
            ',' => tokens.push(Token::Comma),
            '0'..='9' | '.' => {
                let start = index;
                index += 1;
                while index < chars.len()
                    && (chars[index].is_ascii_digit()
                        || matches!(chars[index], '.' | 'e' | 'E' | '+' | '-'))
                {
                    if matches!(chars[index], '+' | '-') && !matches!(chars[index - 1], 'e' | 'E') {
                        break;
                    }
                    index += 1;
                }
                let text = chars[start..index].iter().collect::<String>();
                let value = text.parse::<f64>().map_err(|_| {
                    RepidExpressionError::new(
                        RepidExpressionErrorKind::Syntax,
                        format!("Invalid number '{text}'."),
                    )
                })?;
                if !value.is_finite() {
                    return Err(RepidExpressionError::new(
                        RepidExpressionErrorKind::Syntax,
                        format!("Number '{text}' is not finite."),
                    ));
                }
                tokens.push(Token::Number(value));
                continue;
            }
            _ if ch.is_ascii_alphabetic() || ch == '_' => {
                let start = index;
                index += 1;
                while index < chars.len()
                    && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
                {
                    index += 1;
                }
                let text = chars[start..index].iter().collect::<String>();
                if let Some(channel_number) = parse_channel_token(&text)? {
                    tokens.push(Token::Channel(channel_number));
                } else {
                    tokens.push(Token::Identifier(text));
                }
                continue;
            }
            _ => {
                return Err(RepidExpressionError::new(
                    RepidExpressionErrorKind::Syntax,
                    format!("Unexpected character '{ch}'."),
                ));
            }
        }
        index += 1;
    }
    tokens.push(Token::End);
    Ok(tokens)
}

fn parse_channel_token(text: &str) -> Result<Option<usize>, RepidExpressionError> {
    let Some(number) = text.strip_prefix("Ch").or_else(|| text.strip_prefix("ch")) else {
        return Ok(None);
    };
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(RepidExpressionError::new(
            RepidExpressionErrorKind::InvalidChannel,
            format!("Invalid channel reference '{text}'. Use Ch<number>."),
        ));
    }
    let channel_number = number.parse::<usize>().map_err(|_| {
        RepidExpressionError::new(
            RepidExpressionErrorKind::InvalidChannel,
            format!("Invalid channel reference '{text}'."),
        )
    })?;
    let channel_index = channel_number.checked_sub(1).ok_or_else(|| {
        RepidExpressionError::new(
            RepidExpressionErrorKind::InvalidChannel,
            "Channel references are 1-based; Ch0 is invalid.",
        )
    })?;
    Ok(Some(channel_index))
}

fn collect_input_channels(ast: &RepidAst, output: &mut Vec<usize>) {
    match ast {
        RepidAst::Constant(_) => {}
        RepidAst::Channel(channel) => {
            if !output.contains(channel) {
                output.push(*channel);
            }
        }
        RepidAst::Unary { value, .. } => collect_input_channels(value, output),
        RepidAst::Binary { left, right, .. } => {
            collect_input_channels(left, output);
            collect_input_channels(right, output);
        }
        RepidAst::Function { args, .. } => {
            for arg in args {
                collect_input_channels(arg, output);
            }
        }
    }
}

fn evaluate_ast(
    ast: &RepidAst,
    sample_index: usize,
    inputs: &[&[f32]],
    input_channels: &[usize],
) -> Result<f64, RepidExpressionError> {
    match ast {
        RepidAst::Constant(value) => Ok(*value),
        RepidAst::Channel(channel) => {
            let input_index = input_channels
                .iter()
                .position(|candidate| candidate == channel)
                .ok_or_else(|| {
                    RepidExpressionError::new(
                        RepidExpressionErrorKind::ChannelOutOfRange,
                        format!("Channel Ch{} is not available for evaluation.", channel + 1),
                    )
                })?;
            Ok(inputs[input_index][sample_index] as f64)
        }
        RepidAst::Unary { op, value } => match op {
            UnaryOp::Negate => Ok(-evaluate_ast(value, sample_index, inputs, input_channels)?),
        },
        RepidAst::Binary { op, left, right } => {
            let left = evaluate_ast(left, sample_index, inputs, input_channels)?;
            let right = evaluate_ast(right, sample_index, inputs, input_channels)?;
            match op {
                BinaryOp::Add => Ok(left + right),
                BinaryOp::Subtract => Ok(left - right),
                BinaryOp::Multiply => Ok(left * right),
                BinaryOp::Divide => {
                    if right.abs() <= DIVISION_EPSILON {
                        return Err(RepidExpressionError::new(
                            RepidExpressionErrorKind::DivisionByZero,
                            "Division by zero while evaluating the derived curve.",
                        ));
                    }
                    Ok(left / right)
                }
            }
        }
        RepidAst::Function { function, args } => {
            let values = args
                .iter()
                .map(|arg| evaluate_ast(arg, sample_index, inputs, input_channels))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(evaluate_function(*function, &values))
        }
    }
}

fn evaluate_function(function: RepidFunction, values: &[f64]) -> f64 {
    match function {
        RepidFunction::PseqLl => pseqll(values[0], values[1], values[2]),
        RepidFunction::PseqPh => pseqph(values[0], values[1], values[2]),
        RepidFunction::PseqP => pseqp(
            values[0], values[1], values[2], values[3], values[4], values[5],
        ),
        RepidFunction::PseqQ => pseqq(
            values[0], values[1], values[2], values[3], values[4], values[5],
        ),
        RepidFunction::PseqIp => active_or_reactive_current(
            pseqp(
                values[0], values[1], values[2], values[3], values[4], values[5],
            ),
            values[0],
            values[1],
            values[2],
        ),
        RepidFunction::PseqIq => active_or_reactive_current(
            pseqq(
                values[0], values[1], values[2], values[3], values[4], values[5],
            ),
            values[0],
            values[1],
            values[2],
        ),
        RepidFunction::Rms3 => {
            ((values[0].powi(2) + values[1].powi(2) + values[2].powi(2)) / 3.0).sqrt()
        }
        RepidFunction::Avg => (values[0] + values[1] + values[2]) / 3.0,
        RepidFunction::Abs => values[0].abs(),
    }
}

fn stable_hash_expression(ast: &RepidAst) -> u64 {
    stable_hash_text(&format!("{ast:?}"))
}

fn stable_hash_text(text: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn finite_values(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn pseqll(ua: f64, ub: f64, uc: f64) -> f64 {
    if !finite_values(&[ua, ub, uc]) {
        return f64::NAN;
    }
    (((ua - ub).powi(2) + (ub - uc).powi(2) + (uc - ua).powi(2)) / 3.0).sqrt()
}

fn pseqph(ia: f64, ib: f64, ic: f64) -> f64 {
    if !finite_values(&[ia, ib, ic]) {
        return f64::NAN;
    }
    ((ia.powi(2) + ib.powi(2) + ic.powi(2)) / 3.0).sqrt()
}

fn pseqp(ua: f64, ub: f64, uc: f64, ia: f64, ib: f64, ic: f64) -> f64 {
    if !finite_values(&[ua, ub, uc, ia, ib, ic]) {
        return f64::NAN;
    }
    ua * ia + ub * ib + uc * ic
}

fn pseqq(ua: f64, ub: f64, uc: f64, ia: f64, ib: f64, ic: f64) -> f64 {
    if !finite_values(&[ua, ub, uc, ia, ib, ic]) {
        return f64::NAN;
    }
    ((ub - uc) * ia + (uc - ua) * ib + (ua - ub) * ic) / 3.0_f64.sqrt()
}

fn active_or_reactive_current(power: f64, ua: f64, ub: f64, uc: f64) -> f64 {
    let vll = pseqll(ua, ub, uc);
    let denominator = 3.0_f64.sqrt() * vll;
    if denominator.is_finite() && denominator.abs() > f64::EPSILON {
        power / denominator
    } else {
        f64::NAN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balanced_samples(
        amplitude: f64,
        phase: f64,
        count: usize,
    ) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let sample = |phase_offset: f64| {
            (0..count)
                .map(|index| {
                    let angle = std::f64::consts::TAU * index as f64 / count as f64;
                    (amplitude * (angle + phase + phase_offset).cos()) as f32
                })
                .collect::<Vec<_>>()
        };
        (
            sample(0.0),
            sample(-2.0 * std::f64::consts::PI / 3.0),
            sample(2.0 * std::f64::consts::PI / 3.0),
        )
    }

    #[test]
    fn parses_arithmetic_parentheses_constants_and_channels() {
        let expression = RepidExpression::parse("(Ch1 - Ch2) / 1000").unwrap();

        assert_eq!(expression.input_channels(), &[0, 1]);
        let values = expression
            .evaluate(&[&[1200.0, 1500.0], &[200.0, 500.0]])
            .unwrap();

        assert_eq!(values, vec![1.0, 1.0]);
    }

    #[test]
    fn evaluates_single_channel_scale_and_offset() {
        let expression = RepidExpression::parse("Ch1 * 0.001 + 2").unwrap();
        let values = expression.evaluate(&[&[1000.0, 2500.0]]).unwrap();

        assert_eq!(values, vec![3.0, 4.5]);
    }

    #[test]
    fn evaluates_simple_whitelisted_functions() {
        let rms = RepidExpression::parse("rms3(Ch1, Ch2, Ch3)").unwrap();
        let avg = RepidExpression::parse("avg(Ch1, Ch2, Ch3)").unwrap();
        let abs = RepidExpression::parse("abs(Ch1 - Ch2)").unwrap();
        let inputs = [&[3.0][..], &[4.0][..], &[12.0][..]];

        assert!((rms.evaluate(&inputs).unwrap()[0] - 7.505553).abs() < 0.0001);
        assert!((avg.evaluate(&inputs).unwrap()[0] - 6.333333).abs() < 0.0001);
        assert_eq!(abs.evaluate(&[&[-5.0][..], &[2.0][..]]).unwrap(), vec![7.0]);
    }

    #[test]
    fn rejects_unknown_functions_and_bad_arguments() {
        assert_eq!(
            RepidExpression::parse("exec(Ch1)").unwrap_err().kind(),
            RepidExpressionErrorKind::UnknownFunction
        );
        assert_eq!(
            RepidExpression::parse("pseqll(Ch1,Ch2)")
                .unwrap_err()
                .kind(),
            RepidExpressionErrorKind::WrongArgumentCount
        );
        assert_eq!(
            RepidExpression::parse("pseqll(Ch0,Ch2,Ch3)")
                .unwrap_err()
                .kind(),
            RepidExpressionErrorKind::InvalidChannel
        );
        assert_eq!(
            RepidExpression::parse("pseqll(Ch1,Ch2,Ch3))")
                .unwrap_err()
                .kind(),
            RepidExpressionErrorKind::Syntax
        );
    }

    #[test]
    fn reports_missing_channel_length_mismatch_and_division_by_zero() {
        let expression = RepidExpression::parse("Ch2 + 1").unwrap();
        assert_eq!(
            expression.validate_channel_count(1).unwrap_err().kind(),
            RepidExpressionErrorKind::ChannelOutOfRange
        );
        assert_eq!(
            expression.evaluate(&[]).unwrap_err().kind(),
            RepidExpressionErrorKind::ChannelOutOfRange
        );

        let mismatch = RepidExpression::parse("Ch1 + Ch2").unwrap();
        assert_eq!(
            mismatch
                .evaluate(&[&[1.0, 2.0][..], &[3.0][..]])
                .unwrap_err()
                .kind(),
            RepidExpressionErrorKind::InputLengthMismatch
        );

        let divide = RepidExpression::parse("Ch1 / (Ch2 - Ch2)").unwrap();
        assert_eq!(
            divide
                .evaluate(&[&[1.0][..], &[2.0][..]])
                .unwrap_err()
                .kind(),
            RepidExpressionErrorKind::DivisionByZero
        );
    }

    #[test]
    fn pseqll_and_pseqph_are_balanced_rms_values() {
        let (va, vb, vc) = balanced_samples(220.0_f64 * 2.0_f64.sqrt(), 0.0, 512);
        let voltage = RepidExpression::parse("pseqll(Ch1,Ch2,Ch3)").unwrap();
        let voltage_values = voltage
            .evaluate(&[va.as_slice(), vb.as_slice(), vc.as_slice()])
            .unwrap();

        assert!(voltage_values
            .iter()
            .all(|value| (*value - 381.051).abs() < 0.2));

        let (ia, ib, ic) = balanced_samples(10.0_f64 * 2.0_f64.sqrt(), 0.0, 512);
        let current = RepidExpression::parse("pseqph(Ch1,Ch2,Ch3)").unwrap();
        let current_values = current
            .evaluate(&[ia.as_slice(), ib.as_slice(), ic.as_slice()])
            .unwrap();

        assert!(current_values
            .iter()
            .all(|value| (*value - 10.0).abs() < 0.02));
    }

    #[test]
    fn power_functions_have_expected_sign_and_magnitude() {
        let (va, vb, vc) = balanced_samples(100.0, 0.0, 512);
        let (ia, ib, ic) = balanced_samples(10.0, -30.0_f64.to_radians(), 512);
        let inputs = [
            va.as_slice(),
            vb.as_slice(),
            vc.as_slice(),
            ia.as_slice(),
            ib.as_slice(),
            ic.as_slice(),
        ];

        let p = RepidExpression::parse("pseqp(Ch1,Ch2,Ch3,Ch4,Ch5,Ch6)").unwrap();
        let q = RepidExpression::parse("pseqq(Ch1,Ch2,Ch3,Ch4,Ch5,Ch6)").unwrap();
        let ip = RepidExpression::parse("pseqip(Ch1,Ch2,Ch3,Ch4,Ch5,Ch6)").unwrap();
        let iq = RepidExpression::parse("pseqiq(Ch1,Ch2,Ch3,Ch4,Ch5,Ch6)").unwrap();

        let p_values = p.evaluate(&inputs).unwrap();
        let q_values = q.evaluate(&inputs).unwrap();
        let ip_values = ip.evaluate(&inputs).unwrap();
        let iq_values = iq.evaluate(&inputs).unwrap();

        assert!(p_values.iter().all(|value| (*value - 1299.038).abs() < 0.8));
        assert!(q_values.iter().all(|value| (*value - 750.0).abs() < 0.8));
        assert!(ip_values.iter().all(|value| (*value - 6.123).abs() < 0.02));
        assert!(iq_values.iter().all(|value| (*value - 3.536).abs() < 0.02));
    }

    #[test]
    fn preset_generation_uses_readable_names_and_safe_expressions() {
        let curves =
            positive_sequence_presets([0, 1, 2], [18, 19, 20], RepidPresetBases::default());

        assert_eq!(curves.len(), 5);
        assert_eq!(curves[0].name, "正序电压");
        assert_eq!(curves[0].script, "pseqll(Ch1,Ch2,Ch3) / 220");
        assert!(curves.iter().all(|curve| curve.expression().is_ok()));
    }

    #[test]
    fn cache_signature_hits_and_invalidates_on_expression_inputs_or_transform() {
        let mut curve = RepidDerivedCurve {
            name: "Scaled".to_owned(),
            raw_name: "Scaled".to_owned(),
            script_vars: Vec::new(),
            script: "Ch1 * 0.001 + 2".to_owned(),
            min: None,
            max: None,
            k: 1.0,
            b: 0.0,
            color: None,
            pen_style: None,
            auto_color: true,
            time_offset: 0.0,
            time_scale: 1.0,
        };
        let first = curve.cache_signature();
        assert_eq!(first, curve.cache_signature());

        curve.script = "Ch2 * 0.001 + 2".to_owned();
        let changed_input = curve.cache_signature();
        assert_ne!(first, changed_input);
        assert_eq!(changed_input.expression.input_channels, vec![1]);

        curve.k = 2.0;
        assert_ne!(changed_input, curve.cache_signature());
    }
}
