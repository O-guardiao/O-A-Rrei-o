//! Linguagem de expressões aritméticas para Equality Saturation.

use anyhow::{Context, Result};
use std::fmt;

/// Expressão aritmética simples.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Const(i64),
    Var(String),
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Const(c) => write!(f, "{}", c),
            Expr::Var(v) => write!(f, "{}", v),
            Expr::Add(a, b) => write!(f, "({} + {})", a, b),
            Expr::Mul(a, b) => write!(f, "({} * {})", a, b),
            Expr::Neg(e) => write!(f, "(-{})", e),
        }
    }
}

impl Expr {
    /// Parser simplificado para expressões aritméticas.
    /// Suporta: números, variáveis (identificadores), +, *, - unário, parênteses.
    pub fn parse(s: &str) -> Result<Expr> {
        let tokens = tokenize(s)?;
        let (expr, rest) = parse_expr(&tokens, 0).context("falha ao parsear expressão")?;
        if rest != tokens.len() {
            anyhow::bail!("tokens não consumidos: {:?}", &tokens[rest..]);
        }
        Ok(expr)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(i64),
    Ident(String),
    Plus,
    Star,
    LParen,
    RParen,
    Minus,
}

fn tokenize(s: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                let num = std::str::from_utf8(&bytes[start..i])
                    .unwrap()
                    .parse::<i64>()
                    .context("número inválido")?;
                tokens.push(Token::Num(num));
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < bytes.len() {
                    let ch = bytes[i] as char;
                    if ch.is_alphanumeric() || ch == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let ident = std::str::from_utf8(&bytes[start..i]).unwrap().to_string();
                tokens.push(Token::Ident(ident));
            }
            _ => {
                anyhow::bail!("caractere inesperado: '{}'", c);
            }
        }
    }
    Ok(tokens)
}

fn parse_expr(tokens: &[Token], pos: usize) -> Result<(Expr, usize)> {
    parse_add(tokens, pos)
}

fn parse_add(tokens: &[Token], pos: usize) -> Result<(Expr, usize)> {
    let (mut lhs, mut pos) = parse_mul(tokens, pos)?;
    while pos < tokens.len() {
        match &tokens[pos] {
            Token::Plus => {
                pos += 1;
                let (rhs, next) = parse_mul(tokens, pos)?;
                lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
                pos = next;
            }
            _ => break,
        }
    }
    Ok((lhs, pos))
}

fn parse_mul(tokens: &[Token], pos: usize) -> Result<(Expr, usize)> {
    let (mut lhs, mut pos) = parse_unary(tokens, pos)?;
    while pos < tokens.len() {
        match &tokens[pos] {
            Token::Star => {
                pos += 1;
                let (rhs, next) = parse_unary(tokens, pos)?;
                lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
                pos = next;
            }
            _ => break,
        }
    }
    Ok((lhs, pos))
}

fn parse_unary(tokens: &[Token], pos: usize) -> Result<(Expr, usize)> {
    if pos >= tokens.len() {
        anyhow::bail!("esperado token, encontrado fim da entrada");
    }
    match &tokens[pos] {
        Token::Minus => {
            let (expr, next) = parse_unary(tokens, pos + 1)?;
            Ok((Expr::Neg(Box::new(expr)), next))
        }
        _ => parse_primary(tokens, pos),
    }
}

fn parse_primary(tokens: &[Token], pos: usize) -> Result<(Expr, usize)> {
    if pos >= tokens.len() {
        anyhow::bail!("esperado token, encontrado fim da entrada");
    }
    match &tokens[pos] {
        Token::Num(n) => Ok((Expr::Const(*n), pos + 1)),
        Token::Ident(name) => Ok((Expr::Var(name.clone()), pos + 1)),
        Token::LParen => {
            let (expr, next) = parse_expr(tokens, pos + 1)?;
            if next >= tokens.len() || tokens[next] != Token::RParen {
                anyhow::bail!("esperado ')'");
            }
            Ok((expr, next + 1))
        }
        other => anyhow::bail!("token inesperado: {:?}", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_const() {
        let e = Expr::Const(42);
        let s = e.to_string();
        let parsed = Expr::parse(&s).unwrap();
        assert_eq!(e, parsed);
    }

    #[test]
    fn roundtrip_var() {
        let e = Expr::Var("x".to_string());
        let s = e.to_string();
        let parsed = Expr::parse(&s).unwrap();
        assert_eq!(e, parsed);
    }

    #[test]
    fn roundtrip_add() {
        let e = Expr::Add(Box::new(Expr::Const(1)), Box::new(Expr::Const(2)));
        let s = e.to_string();
        let parsed = Expr::parse(&s).unwrap();
        assert_eq!(e, parsed);
    }

    #[test]
    fn roundtrip_mul() {
        let e = Expr::Mul(Box::new(Expr::Const(3)), Box::new(Expr::Const(4)));
        let s = e.to_string();
        let parsed = Expr::parse(&s).unwrap();
        assert_eq!(e, parsed);
    }

    #[test]
    fn roundtrip_neg() {
        let e = Expr::Neg(Box::new(Expr::Const(5)));
        let s = e.to_string();
        let parsed = Expr::parse(&s).unwrap();
        assert_eq!(e, parsed);
    }

    #[test]
    fn parse_complex_expr() {
        let s = "(a + (b * 3))";
        let e = Expr::parse(s).unwrap();
        assert_eq!(
            e,
            Expr::Add(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Mul(
                    Box::new(Expr::Var("b".to_string())),
                    Box::new(Expr::Const(3))
                ))
            )
        );
    }

    #[test]
    fn parse_nested_parens() {
        let s = "((x + y) * (2 + z))";
        let e = Expr::parse(s).unwrap();
        assert_eq!(
            e,
            Expr::Mul(
                Box::new(Expr::Add(
                    Box::new(Expr::Var("x".to_string())),
                    Box::new(Expr::Var("y".to_string()))
                )),
                Box::new(Expr::Add(
                    Box::new(Expr::Const(2)),
                    Box::new(Expr::Var("z".to_string()))
                ))
            )
        );
    }

    #[test]
    fn parse_unary_neg() {
        let s = "(-x)";
        let e = Expr::parse(s).unwrap();
        assert_eq!(e, Expr::Neg(Box::new(Expr::Var("x".to_string()))));
    }

    #[test]
    fn parse_precedence() {
        let s = "a + b * c";
        let e = Expr::parse(s).unwrap();
        assert_eq!(
            e,
            Expr::Add(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Mul(
                    Box::new(Expr::Var("b".to_string())),
                    Box::new(Expr::Var("c".to_string()))
                ))
            )
        );
    }

    #[test]
    fn parse_identity_like() {
        let s = "x + 0";
        let e = Expr::parse(s).unwrap();
        assert_eq!(
            e,
            Expr::Add(
                Box::new(Expr::Var("x".to_string())),
                Box::new(Expr::Const(0))
            )
        );
    }

    #[test]
    fn parse_distributivity() {
        let s = "a * (b + c)";
        let e = Expr::parse(s).unwrap();
        assert_eq!(
            e,
            Expr::Mul(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Add(
                    Box::new(Expr::Var("b".to_string())),
                    Box::new(Expr::Var("c".to_string()))
                ))
            )
        );
    }

    #[test]
    fn parse_associativity() {
        let s = "(a + b) + c";
        let e = Expr::parse(s).unwrap();
        assert_eq!(
            e,
            Expr::Add(
                Box::new(Expr::Add(
                    Box::new(Expr::Var("a".to_string())),
                    Box::new(Expr::Var("b".to_string()))
                )),
                Box::new(Expr::Var("c".to_string()))
            )
        );
    }
}
