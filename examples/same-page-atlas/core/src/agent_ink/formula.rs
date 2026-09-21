//! A small, hand-written expression evaluator for `formula` relations.
//!
//! `solve.rs` finds every gradient by nudging a variable and re-evaluating
//! the whole objective (central differences, see `System::gradient`), so
//! this module never needs to differentiate anything symbolically: it only
//! needs to parse an expression once, at relation-create time, and evaluate
//! it as many times as the solver asks. No dependency was added for this —
//! the grammar below is small enough (four arithmetic operators, unary
//! minus, parentheses, `^`, and eight named functions) that a recursive-
//! descent parser is less code than wiring in and constraining a general
//! math-expression crate would have been.
//!
//! Variables are resolved by name against whatever value map the caller
//! passes to [`eval`]; this module knows nothing about agent-ink variables
//! or the atlas at all, which keeps it independently testable.
//!
//! Agent-ink variable names routinely contain spaces ("barrier width",
//! "transmitted amplitude"), which a bare identifier token cannot spell
//! unambiguously — `barrier width` as two tokens looks like two variables
//! multiplied, or a parse error, depending on what follows. A name that
//! needs a space is written inside backticks instead: `` `barrier width` ``.
//! Backticks were chosen over single or double quotes because an
//! expression already travels through JSON (which uses `"`) and often a
//! shell on its way in (which uses both `"` and `'`); backticks are the one
//! delimiter neither layer is likely to have already consumed or escaped.
//! A bare, unquoted identifier keeps meaning exactly what it always has.

use std::collections::BTreeMap;

use crate::AtlasError;

/// Every function name a formula expression may call. Each takes exactly
/// one argument; there is no variadic or multi-argument form in this
/// grammar.
const FUNCTIONS: &[&str] = &["exp", "ln", "log10", "sqrt", "abs", "sin", "cos", "tan"];

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    Var(String),
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Pow(Box<Expr>, Box<Expr>),
    Call(&'static str, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    /// A bare identifier. May still turn out to be a function call, if
    /// it's immediately followed by `(` — see `parse_atom`.
    Ident(String),
    /// A backtick-quoted identifier: always a variable reference, never a
    /// function call, no matter what follows it (a quoted name that
    /// happened to precede a stray `(` should be a parse error at that
    /// `(`, not a silent function-call reinterpretation of the name).
    QuotedIdent(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

impl Token {
    /// How this token would be spelled back out, for error messages that
    /// name the offending token rather than describing it in the abstract.
    fn display(&self) -> String {
        match self {
            Token::Num(value) => value.to_string(),
            Token::Ident(name) => name.clone(),
            Token::QuotedIdent(name) => format!("`{name}`"),
            Token::Plus => "+".to_string(),
            Token::Minus => "-".to_string(),
            Token::Star => "*".to_string(),
            Token::Slash => "/".to_string(),
            Token::Caret => "^".to_string(),
            Token::LParen => "(".to_string(),
            Token::RParen => ")".to_string(),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, AtlasError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        match ch {
            '+' => {
                tokens.push(Token::Plus);
                index += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                index += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                index += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                index += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
                index += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                index += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                index += 1;
            }
            '`' => {
                let start = index;
                index += 1;
                let content_start = index;
                while index < chars.len() && chars[index] != '`' {
                    index += 1;
                }
                if index >= chars.len() {
                    // There is no closing token to name, so the error names
                    // the position instead: the character offset of the
                    // opening backtick that never found its match.
                    return Err(format!(
                        "formula expression has an unterminated quoted variable name starting at character {start}"
                    ));
                }
                let literal: String = chars[content_start..index].iter().collect();
                index += 1;
                if literal.is_empty() {
                    return Err(format!(
                        "formula expression has an empty quoted variable name starting at character {start}"
                    ));
                }
                tokens.push(Token::QuotedIdent(literal));
            }
            c if c.is_ascii_digit() || c == '.' => {
                let start = index;
                index += 1;
                while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.')
                {
                    index += 1;
                }
                if index < chars.len() && (chars[index] == 'e' || chars[index] == 'E') {
                    let mark = index;
                    index += 1;
                    if index < chars.len() && (chars[index] == '+' || chars[index] == '-') {
                        index += 1;
                    }
                    if index < chars.len() && chars[index].is_ascii_digit() {
                        while index < chars.len() && chars[index].is_ascii_digit() {
                            index += 1;
                        }
                    } else {
                        // No exponent digits followed the `e`/`E`: that
                        // suffix is not part of the number after all, so
                        // back out of it and let the token end here.
                        index = mark;
                    }
                }
                let literal: String = chars[start..index].iter().collect();
                let value = literal.parse::<f64>().map_err(|_| {
                    format!("formula expression has a malformed number {literal:?}")
                })?;
                tokens.push(Token::Num(value));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = index;
                index += 1;
                while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_')
                {
                    index += 1;
                }
                let literal: String = chars[start..index].iter().collect();
                tokens.push(Token::Ident(literal));
            }
            other => {
                return Err(format!(
                    "formula expression has an unrecognized character {other:?}"
                ))
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        self.position += 1;
        token
    }

    fn expect(&mut self, expected: &Token) -> Result<(), AtlasError> {
        match self.advance() {
            Some(token) if token == *expected => Ok(()),
            Some(token) => Err(format!(
                "formula expression expected {:?} but found {:?}",
                expected.display(),
                token.display()
            )),
            None => Err(format!(
                "formula expression ended before finding the closing {:?} it needed",
                expected.display()
            )),
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, AtlasError> {
        let mut left = self.parse_term()?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.advance();
                    let right = self.parse_term()?;
                    left = Expr::Add(Box::new(left), Box::new(right));
                }
                Some(Token::Minus) => {
                    self.advance();
                    let right = self.parse_term()?;
                    left = Expr::Sub(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<Expr, AtlasError> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Token::Star) => {
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::Mul(Box::new(left), Box::new(right));
                }
                Some(Token::Slash) => {
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::Div(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, AtlasError> {
        match self.peek() {
            Some(Token::Minus) => {
                self.advance();
                Ok(Expr::Neg(Box::new(self.parse_unary()?)))
            }
            Some(Token::Plus) => {
                self.advance();
                self.parse_unary()
            }
            _ => self.parse_power(),
        }
    }

    /// `^` binds tighter than unary minus on its left (`-2^2` is `-(2^2)`,
    /// handled by `parse_unary` calling this) and is right-associative on
    /// its right (`2^-3` recurses back into `parse_unary` for the
    /// exponent, so a second leading minus is legal there).
    fn parse_power(&mut self) -> Result<Expr, AtlasError> {
        let base = self.parse_atom()?;
        if matches!(self.peek(), Some(Token::Caret)) {
            self.advance();
            let exponent = self.parse_unary()?;
            Ok(Expr::Pow(Box::new(base), Box::new(exponent)))
        } else {
            Ok(base)
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, AtlasError> {
        match self.advance() {
            Some(Token::Num(value)) => Ok(Expr::Num(value)),
            Some(Token::LParen) => {
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            Some(Token::Ident(name)) => {
                if matches!(self.peek(), Some(Token::LParen)) {
                    let function = FUNCTIONS.iter().find(|candidate| **candidate == name);
                    let Some(&function) = function else {
                        return Err(format!(
                            "formula expression calls unknown function {name:?}"
                        ));
                    };
                    self.advance();
                    let argument = self.parse_expr()?;
                    self.expect(&Token::RParen)?;
                    Ok(Expr::Call(function, Box::new(argument)))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            // A backtick-quoted name is always a variable, even one that
            // happens to spell a function name or is followed by `(` — the
            // quoting is what the author used to say "this exact string is
            // one variable", and that should never be second-guessed into
            // a function call.
            Some(Token::QuotedIdent(name)) => Ok(Expr::Var(name)),
            Some(token) => Err(format!(
                "formula expression did not expect {:?} here",
                token.display()
            )),
            None => Err("formula expression ended where a value was expected".to_string()),
        }
    }
}

/// Parse a formula expression, refusing anything it cannot fully consume.
///
/// A caller with a token it cannot place — an unknown function name, a
/// dangling operator, unbalanced parentheses, trailing input after a
/// complete expression — gets an error that names that exact token, so a
/// relation can never be created from an expression the evaluator would
/// silently misread.
pub fn parse(source: &str) -> Result<Expr, AtlasError> {
    let tokens = tokenize(source)?;
    if tokens.is_empty() {
        return Err("formula expression cannot be empty".to_string());
    }
    let mut parser = Parser {
        tokens,
        position: 0,
    };
    let expr = parser.parse_expr()?;
    if let Some(remainder) = parser.peek() {
        return Err(format!(
            "formula expression has unexpected trailing input starting at {:?}",
            remainder.display()
        ));
    }
    Ok(expr)
}

/// Every variable name the expression references, in first-seen order.
/// Duplicates collapse: a name used twice is one name to resolve, not two.
pub fn variables(expr: &Expr) -> Vec<String> {
    let mut seen = Vec::new();
    collect_variables(expr, &mut seen);
    seen
}

fn collect_variables(expr: &Expr, seen: &mut Vec<String>) {
    match expr {
        Expr::Num(_) => {}
        Expr::Var(name) => {
            if !seen.contains(name) {
                seen.push(name.clone());
            }
        }
        Expr::Neg(inner) | Expr::Call(_, inner) => collect_variables(inner, seen),
        Expr::Add(left, right)
        | Expr::Sub(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right)
        | Expr::Pow(left, right) => {
            collect_variables(left, seen);
            collect_variables(right, seen);
        }
    }
}

/// Evaluate a parsed expression against a name-to-value map.
///
/// This never panics and never returns an error: division by zero,
/// `ln` of a negative number, and similar out-of-domain evaluations follow
/// ordinary IEEE-754 rules and come back as `inf`, `-inf`, or `NaN`. It is
/// `solve.rs`'s job to decide what a non-finite residual means for the
/// solve as a whole (see the comment beside `ResidualKind::Formula`); this
/// function's only job is to compute honestly what the expression says.
///
/// A variable name missing from `values` cannot happen through the normal
/// create-relation path (every name in the expression is checked against
/// the declared members there), but returns `NaN` rather than panicking if
/// it ever does, for the same reason a non-finite domain error does: a
/// silently wrong number is worse, but a solve that panics is worse still.
pub fn eval(expr: &Expr, values: &BTreeMap<&str, f64>) -> f64 {
    match expr {
        Expr::Num(value) => *value,
        Expr::Var(name) => values.get(name.as_str()).copied().unwrap_or(f64::NAN),
        Expr::Neg(inner) => -eval(inner, values),
        Expr::Add(left, right) => eval(left, values) + eval(right, values),
        Expr::Sub(left, right) => eval(left, values) - eval(right, values),
        Expr::Mul(left, right) => eval(left, values) * eval(right, values),
        Expr::Div(left, right) => eval(left, values) / eval(right, values),
        Expr::Pow(left, right) => eval(left, values).powf(eval(right, values)),
        Expr::Call(function, inner) => {
            let value = eval(inner, values);
            match *function {
                "exp" => value.exp(),
                "ln" => value.ln(),
                "log10" => value.log10(),
                "sqrt" => value.sqrt(),
                "abs" => value.abs(),
                "sin" => value.sin(),
                "cos" => value.cos(),
                "tan" => value.tan(),
                _ => unreachable!("parse() only ever builds Call nodes from FUNCTIONS"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_with(source: &str, bindings: &[(&str, f64)]) -> f64 {
        let expr = parse(source).expect("parse expression");
        let values = bindings
            .iter()
            .map(|(name, value)| (*name, *value))
            .collect();
        eval(&expr, &values)
    }

    #[test]
    fn arithmetic_precedence_and_unary_minus_match_ordinary_convention() {
        assert_eq!(eval_with("2 + 3 * 4", &[]), 14.0);
        assert_eq!(eval_with("(2 + 3) * 4", &[]), 20.0);
        assert_eq!(eval_with("-2 ^ 2", &[]), -4.0);
        assert_eq!(eval_with("2 ^ -1", &[]), 0.5);
    }

    #[test]
    fn functions_and_variables_resolve_by_name() {
        assert!((eval_with("sqrt(x)", &[("x", 9.0)]) - 3.0).abs() < 1e-12);
        assert!((eval_with("exp(-2*x)", &[("x", 0.0)]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn unknown_function_names_are_refused() {
        let error = parse("frobnicate(x)").unwrap_err();
        assert!(error.contains("frobnicate"), "{error}");
    }

    #[test]
    fn unbalanced_parentheses_are_refused() {
        let error = parse("(1 + 2").unwrap_err();
        assert!(error.contains(")"), "{error}");
    }

    #[test]
    fn trailing_input_is_refused() {
        let error = parse("1 + 2 3").unwrap_err();
        assert!(error.contains("3"), "{error}");
    }

    #[test]
    fn variables_are_listed_once_each_in_first_seen_order() {
        let expr = parse("x + y * x").expect("parse expression");
        assert_eq!(variables(&expr), vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn a_backtick_quoted_name_with_a_space_resolves_like_any_other_variable() {
        let value = eval_with(
            "120 * exp(-0.03 * `barrier width`)",
            &[("barrier width", 10.0)],
        );
        let expected = 120.0 * (-0.03_f64 * 10.0).exp();
        assert!((value - expected).abs() < 1e-12, "{value} vs {expected}");

        let expr = parse("`barrier width` + `barrier width`").expect("parse expression");
        assert_eq!(variables(&expr), vec!["barrier width".to_string()]);
    }

    #[test]
    fn an_unterminated_quote_is_refused_naming_the_position() {
        let error = parse("1 + `barrier width * 2").unwrap_err();
        eprintln!("unterminated_quote_error={error}");
        assert!(error.contains("unterminated"), "{error}");
        // The backtick that never closed starts at character 4 ("1 + `..."),
        // and naming that position is the only way to point at the mistake
        // when there is no closing token to name instead.
        assert!(error.contains('4'), "{error}");
    }

    #[test]
    fn an_empty_quoted_name_is_refused() {
        let error = parse("`` + 1").unwrap_err();
        assert!(error.contains("empty"), "{error}");
    }

    #[test]
    fn a_quoted_name_is_never_reinterpreted_as_a_function_call() {
        // ``sin`(2)`` parses as `Var("sin")` followed by unconsumed `(2)`:
        // the quoting forces "sin" to be read as a variable, so the `(`
        // that follows is trailing input to refuse, never a call to accept.
        let error = parse("`sin`(2)").unwrap_err();
        assert!(error.contains('('), "{error}");
    }
}
