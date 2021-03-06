//! Parsing from tokens to ASTs.

use crate::ast::{
  Arm, Cases, ConBind, DatBind, Dec, ExBind, ExBindInner, ExDesc, Exp, FValBind, FValBindCase,
  FunBind, Label, Long, Pat, Row, SigBind, SigExp, Spec, StrBind, StrDec, StrDesc, StrExp, TopDec,
  Ty, TyBind, TyDesc, TyPrec, ValBind, ValDesc,
};
use crate::intern::{StrRef, StrStore};
use crate::lex::Lexer;
use crate::loc::{Loc, Located};
use crate::token::{IdentType, IsNumLab, Token, TyVar};
use maplit::hashmap;
use std::collections::HashMap;
use std::convert::TryInto as _;

/// A specialized Result that most functions in this module return.
pub type Result<T> = std::result::Result<T, Located<Error>>;

/// Parse the tokens in the Lexer into a sequence of top-level definitions.
pub fn get(lexer: Lexer) -> Result<Vec<Located<TopDec<StrRef>>>> {
  let mut ret = Vec::new();
  let last_loc = match lexer.last_loc() {
    Some(x) => x,
    None => return Ok(ret),
  };
  let mut p = Parser::new(lexer, last_loc);
  loop {
    if let Token::EOF = p.peek().val {
      break;
    }
    ret.push(p.top_dec()?);
  }
  ret.shrink_to_fit();
  Ok(ret)
}

/// An error emitted when parsing.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum Error {
  ExpectedButFound(&'static str, &'static str),
  InfixWithoutOp(StrRef),
  NotInfix(StrRef),
  RealPat,
  NegativeFixity,
  SameFixityDiffAssoc,
}

impl Error {
  /// A human-readable message describing this error.
  pub fn message(&self, store: &StrStore) -> String {
    match self {
      Self::ExpectedButFound(exp, fnd) => format!("expected {}, found {}", exp, fnd),
      Self::InfixWithoutOp(id) => format!(
        "infix identifier used without preceding `op`: {}",
        store.get(*id)
      ),
      Self::NotInfix(id) => format!("non-infix identifier used as infix: {}", store.get(*id)),
      Self::RealPat => "real constant used as a pattern".to_owned(),
      Self::NegativeFixity => "fixity is negative".to_owned(),
      Self::SameFixityDiffAssoc => {
        "consecutive infix identifiers with same fixity but different associativity".to_owned()
      }
    }
  }
}

struct Parser {
  lexer: Lexer,
  i: usize,
  ops: HashMap<StrRef, OpInfo>,
  last_loc: Loc,
}

// NOTE the `maybe` family of functions return Result<Option<T>>. these functions return:
// - Ok(Some(..)) if they did parse a T
// - Ok(None) if they couldn't parse a T but didn't consume any tokens
// - Err(..) if they couldn't parse a T but did consume tokens

impl Parser {
  /// constructs a new Parser.
  fn new(lexer: Lexer, last_loc: Loc) -> Self {
    Self {
      lexer,
      last_loc,
      i: 0,
      ops: hashmap![
        StrRef::CONS => OpInfo::right(5),
        StrRef::EQ => OpInfo::left(4),
        StrRef::ASSIGN => OpInfo::left(3),
        StrRef::DIV => OpInfo::left(7),
        StrRef::MOD => OpInfo::left(7),
        StrRef::STAR => OpInfo::left(7),
        StrRef::SLASH => OpInfo::left(7),
        StrRef::PLUS => OpInfo::left(6),
        StrRef::MINUS => OpInfo::left(6),
        StrRef::LT => OpInfo::left(4),
        StrRef::GT => OpInfo::left(4),
        StrRef::LT_EQ => OpInfo::left(4),
        StrRef::GT_EQ => OpInfo::left(4),
      ],
    }
  }

  /// gets the current token. does not advance the parser.
  fn peek(&self) -> Located<Token> {
    match self.lexer.get(self.i) {
      Some(tok) => tok,
      None => self.last_loc.wrap(Token::EOF),
    }
  }

  /// advances the parser ahead 1 token.
  #[inline]
  fn skip(&mut self) {
    self.i += 1;
  }

  /// combines a 'begin' loc with the 'end' loc, which is the loc of the last token we consumed, and
  /// uses it to wrap val.
  fn wrap<T>(&self, begin: Loc, val: T) -> Located<T> {
    let end = match self.lexer.get(self.i - 1) {
      Some(tok) => tok.loc,
      None => self.last_loc,
    };
    begin.span(end).wrap(val)
  }

  /// if the current token is `tok`, return `Ok(())` and advance, else return `Err(..)`.
  fn eat(&mut self, tok: Token) -> Result<()> {
    let next = self.peek();
    if next.val == tok {
      self.skip();
      Ok(())
    } else {
      self.fail(tok.desc(), next)
    }
  }

  /// returns an ExpectedButFound error, where we expected `want` but got `tok`.
  fn fail<T>(&mut self, want: &'static str, tok: Located<Token>) -> Result<T> {
    Err(tok.loc.wrap(Error::ExpectedButFound(want, tok.val.desc())))
  }

  fn top_dec(&mut self) -> Result<Located<TopDec<StrRef>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let ret = match tok.val {
      Token::Signature => {
        self.skip();
        let mut sig_binds = Vec::new();
        loop {
          let id = self.alpha_num_id()?;
          self.eat(Token::Equal)?;
          let exp = self.sig_exp()?;
          sig_binds.push(SigBind { id, exp });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        sig_binds.shrink_to_fit();
        TopDec::SigDec(sig_binds)
      }
      Token::Functor => {
        self.skip();
        let mut fun_binds = Vec::new();
        loop {
          let fun_id = self.alpha_num_id()?;
          self.eat(Token::LRound)?;
          let str_id = self.alpha_num_id()?;
          self.eat(Token::Colon)?;
          let sig_exp = self.sig_exp()?;
          self.eat(Token::RRound)?;
          let str_exp = self.str_exp_sugar()?;
          fun_binds.push(FunBind {
            fun_id,
            str_id,
            sig_exp,
            str_exp,
          });
          if let Token::And = self.peek().val {
            continue;
          } else {
            break;
          }
        }
        fun_binds.shrink_to_fit();
        TopDec::FunDec(fun_binds)
      }
      _ => {
        let sd = self.str_dec()?;
        if let StrDec::Seq(ref xs) = sd.val {
          if xs.is_empty() {
            return self.fail("a top-level declaration", self.peek());
          }
        }
        TopDec::StrDec(sd)
      }
    };
    Ok(self.wrap(begin, ret))
  }

  fn str_exp_sugar(&mut self) -> Result<Located<StrExp<StrRef>>> {
    let sig_exp = match self.peek().val {
      Token::Colon => {
        self.skip();
        Some((self.sig_exp()?, false))
      }
      Token::ColonGt => {
        self.skip();
        Some((self.sig_exp()?, true))
      }
      _ => None,
    };
    self.eat(Token::Equal)?;
    let mut ret = self.str_exp()?;
    if let Some((se, opaque)) = sig_exp {
      ret = ret.loc.wrap(StrExp::Ascription(ret.into(), se, opaque));
    }
    Ok(ret)
  }

  fn str_exp(&mut self) -> Result<Located<StrExp<StrRef>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let mut ret = match tok.val {
      Token::Struct => {
        self.skip();
        let ops = self.ops.clone();
        let dec = self.str_dec()?;
        self.eat(Token::End)?;
        self.ops = ops;
        StrExp::Struct(dec)
      }
      Token::Let => {
        self.skip();
        let ops = self.ops.clone();
        let dec = self.str_dec()?;
        self.eat(Token::In)?;
        let exp = self.str_exp()?;
        self.eat(Token::End)?;
        self.ops = ops;
        StrExp::Let(dec, exp.into())
      }
      Token::Ident(id, IdentType::AlphaNum) => {
        self.skip();
        if let Token::LRound = self.peek().val {
          self.skip();
          let exp = match self.maybe_str_dec()? {
            Some(sd) => sd.loc.wrap(StrExp::Struct(sd)),
            None => self.str_exp()?,
          };
          self.eat(Token::RRound)?;
          StrExp::FunctorApp(begin.wrap(id), exp.into())
        } else {
          self.i -= 1;
          let long_id = self.long_alpha_num_id()?;
          StrExp::LongStrId(long_id)
        }
      }
      _ => return self.fail("a structure expression", tok),
    };
    loop {
      ret = match self.peek().val {
        Token::Colon => {
          self.skip();
          let exp = self.sig_exp()?;
          StrExp::Ascription(self.wrap(begin, ret).into(), exp, false)
        }
        Token::ColonGt => {
          self.skip();
          let exp = self.sig_exp()?;
          StrExp::Ascription(self.wrap(begin, ret).into(), exp, true)
        }
        _ => break,
      };
    }
    Ok(self.wrap(begin, ret))
  }

  fn maybe_str_dec(&mut self) -> Result<Option<Located<StrDec<StrRef>>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let ret = match tok.val {
      Token::Structure => {
        self.skip();
        let mut str_binds = Vec::new();
        loop {
          let id = self.alpha_num_id()?;
          let exp = self.str_exp_sugar()?;
          str_binds.push(StrBind { id, exp });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        str_binds.shrink_to_fit();
        StrDec::Structure(str_binds)
      }
      Token::Local => {
        self.skip();
        let ops = self.ops.clone();
        let fst = self.str_dec()?;
        self.eat(Token::In)?;
        let snd = self.str_dec()?;
        self.eat(Token::End)?;
        self.ops = ops;
        StrDec::Local(fst.into(), snd.into())
      }
      _ => {
        let dec = self.dec()?;
        if let Dec::Seq(ref xs) = dec.val {
          if xs.is_empty() {
            return Ok(None);
          }
        }
        StrDec::Dec(dec)
      }
    };
    Ok(Some(self.wrap(begin, ret)))
  }

  fn str_dec(&mut self) -> Result<Located<StrDec<StrRef>>> {
    self.semicolon_seq(Self::maybe_str_dec, StrDec::Seq)
  }

  fn sig_exp(&mut self) -> Result<Located<SigExp<StrRef>>> {
    let tok = self.peek();
    let begin = tok.loc;
    self.skip();
    let mut ret = match tok.val {
      Token::Sig => {
        let spec = self.spec()?;
        self.eat(Token::End)?;
        SigExp::Sig(spec)
      }
      Token::Ident(id, IdentType::AlphaNum) => SigExp::SigId(begin.wrap(id)),
      _ => return self.fail("a signature expression", tok),
    };
    while let Token::Where = self.peek().val {
      self.skip();
      let ty_vars = self.ty_var_seq()?;
      let ty_con = self.long_id(true)?;
      self.eat(Token::Equal)?;
      let ty = self.ty()?;
      ret = SigExp::Where(self.wrap(begin, ret).into(), ty_vars, ty_con, ty);
    }
    Ok(self.wrap(begin, ret))
  }

  fn maybe_spec(&mut self) -> Result<Option<Located<Spec<StrRef>>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let mut ret = match tok.val {
      Token::Val => {
        self.skip();
        let mut val_descs = Vec::new();
        loop {
          let vid = self.ident()?;
          self.eat(Token::Colon)?;
          let ty = self.ty()?;
          val_descs.push(ValDesc { vid, ty });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        val_descs.shrink_to_fit();
        Spec::Val(val_descs)
      }
      Token::Type => {
        self.skip();
        Spec::Type(self.ty_descs()?, false)
      }
      Token::Eqtype => {
        self.skip();
        Spec::Type(self.ty_descs()?, true)
      }
      Token::Datatype => match self.datatype_dec(false)? {
        DatatypeDec::Binds(dat_binds) => Spec::Datatype(dat_binds),
        DatatypeDec::Copy(ty_con, long) => Spec::DatatypeCopy(ty_con, long),
      },
      Token::Exception => {
        self.skip();
        let mut ex_descs = Vec::new();
        loop {
          let vid = self.ident()?;
          let ty = self.maybe_of_ty()?;
          ex_descs.push(ExDesc { vid, ty });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        ex_descs.shrink_to_fit();
        Spec::Exception(ex_descs)
      }
      Token::Structure => {
        self.skip();
        let mut str_descs = Vec::new();
        loop {
          let str_id = self.alpha_num_id()?;
          self.eat(Token::Colon)?;
          let exp = self.sig_exp()?;
          str_descs.push(StrDesc { str_id, exp });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        str_descs.shrink_to_fit();
        Spec::Structure(str_descs)
      }
      Token::Include => {
        self.skip();
        let mut sig_ids = Vec::new();
        loop {
          let tok = self.peek();
          if let Token::Ident(id, IdentType::AlphaNum) = tok.val {
            self.skip();
            let loc = tok.loc;
            sig_ids.push(loc.wrap(Spec::Include(loc.wrap(SigExp::SigId(loc.wrap(id))).into())));
          } else {
            break;
          }
        }
        if sig_ids.is_empty() {
          let exp = self.sig_exp()?;
          Spec::Include(exp.into())
        } else {
          sig_ids.shrink_to_fit();
          Spec::Seq(sig_ids)
        }
      }
      _ => return Ok(None),
    };
    while let Token::Sharing = self.peek().val {
      self.skip();
      self.eat(Token::Type)?;
      let mut ty_cons = Vec::new();
      loop {
        ty_cons.push(self.long_id(true)?);
        if let Token::Equal = self.peek().val {
          self.skip();
        } else {
          break;
        }
      }
      if ty_cons.len() < 2 {
        return self.fail("an identifier", self.peek());
      }
      ty_cons.shrink_to_fit();
      ret = Spec::Sharing(self.wrap(begin, ret).into(), ty_cons);
    }
    Ok(Some(self.wrap(begin, ret)))
  }

  fn ty_descs(&mut self) -> Result<Vec<TyDesc<StrRef>>> {
    let mut ret = Vec::new();
    loop {
      let ty_vars = self.ty_var_seq()?;
      let ty_con = self.ident()?;
      ret.push(TyDesc { ty_vars, ty_con });
      if let Token::And = self.peek().val {
        self.skip();
      } else {
        break;
      }
    }
    ret.shrink_to_fit();
    Ok(ret)
  }

  fn spec(&mut self) -> Result<Located<Spec<StrRef>>> {
    self.semicolon_seq(Self::maybe_spec, Spec::Seq)
  }

  fn maybe_at_exp(&mut self) -> Result<Option<Located<Exp<StrRef>>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let ret = match tok.val {
      Token::DecInt(n, _) => {
        self.skip();
        Exp::DecInt(n)
      }
      Token::HexInt(n) => {
        self.skip();
        Exp::HexInt(n)
      }
      Token::DecWord(n) => {
        self.skip();
        Exp::DecWord(n)
      }
      Token::HexWord(n) => {
        self.skip();
        Exp::HexWord(n)
      }
      Token::Real(n) => {
        self.skip();
        Exp::Real(n)
      }
      Token::String(s) => {
        self.skip();
        Exp::String(s)
      }
      Token::Char(c) => {
        self.skip();
        Exp::Char(c)
      }
      Token::Op => {
        self.skip();
        Exp::LongVid(self.long_id(true)?)
      }
      Token::LCurly => {
        self.skip();
        let mut rows = Vec::new();
        if let Token::RCurly = self.peek().val {
          self.skip();
        } else {
          loop {
            let lab = self.label()?;
            self.eat(Token::Equal)?;
            let val = self.exp()?;
            rows.push(Row { lab, val });
            let tok = self.peek();
            self.skip();
            match tok.val {
              Token::RCurly => break,
              Token::Comma => continue,
              _ => return self.fail("`}` or `,`", tok),
            }
          }
        }
        rows.shrink_to_fit();
        Exp::Record(rows)
      }
      Token::Pound => {
        self.skip();
        Exp::Select(self.label()?)
      }
      Token::LRound => {
        self.skip();
        if let Token::RRound = self.peek().val {
          self.skip();
          return Ok(Some(self.wrap(begin, Exp::Tuple(Vec::new()))));
        }
        let fst = self.exp()?;
        let tok = self.peek();
        self.skip();
        match tok.val {
          Token::RRound => fst.val,
          Token::Comma => {
            let mut exprs = vec![fst];
            loop {
              exprs.push(self.exp()?);
              let tok = self.peek();
              self.skip();
              match tok.val {
                Token::RRound => break,
                Token::Comma => continue,
                _ => return self.fail("`)` or `,`", tok),
              }
            }
            Exp::Tuple(exprs)
          }
          Token::Semicolon => {
            let mut exprs = vec![fst];
            loop {
              exprs.push(self.exp()?);
              let tok = self.peek();
              self.skip();
              match tok.val {
                Token::RRound => break,
                Token::Semicolon => continue,
                _ => return self.fail("`)` or `;`", tok),
              }
            }
            Exp::Sequence(exprs)
          }
          _ => return self.fail("`)`, `,`, or `;`", tok),
        }
      }
      Token::LSquare => {
        self.skip();
        let mut exprs = Vec::new();
        if let Token::RSquare = self.peek().val {
          self.skip();
        } else {
          loop {
            exprs.push(self.exp()?);
            let tok = self.peek();
            self.skip();
            match tok.val {
              Token::RSquare => break,
              Token::Comma => continue,
              _ => return self.fail("`]` or `,`", tok),
            }
          }
        }
        exprs.shrink_to_fit();
        Exp::List(exprs)
      }
      Token::Let => {
        self.skip();
        let ops = self.ops.clone();
        let dec = self.dec()?;
        self.eat(Token::In)?;
        let mut exprs = Vec::new();
        loop {
          exprs.push(self.exp()?);
          let tok = self.peek();
          self.skip();
          match tok.val {
            Token::End => break,
            Token::Semicolon => continue,
            _ => return self.fail("`end` or `;`", tok),
          }
        }
        self.ops = ops;
        exprs.shrink_to_fit();
        Exp::Let(dec, exprs)
      }
      Token::Ident(..) => Exp::LongVid(self.long_id(false)?),
      Token::Equal => {
        // someone decided to 'nonfix ='.
        self.skip();
        Exp::LongVid(Long {
          structures: vec![],
          last: tok.loc.wrap(StrRef::EQ),
        })
      }
      _ => return Ok(None),
    };
    Ok(Some(self.wrap(begin, ret)))
  }

  fn at_exp(&mut self) -> Result<Located<Exp<StrRef>>> {
    match self.maybe_at_exp()? {
      Some(x) => Ok(x),
      None => {
        let tok = self.peek();
        self.fail("an expression", tok)
      }
    }
  }

  fn ident(&mut self) -> Result<Located<StrRef>> {
    let tok = self.peek();
    self.skip();
    if let Token::Ident(id, _) = tok.val {
      Ok(tok.loc.wrap(id))
    } else {
      self.fail("an identifier", tok)
    }
  }

  fn alpha_num_id(&mut self) -> Result<Located<StrRef>> {
    let tok = self.peek();
    if let Token::Ident(id, IdentType::AlphaNum) = tok.val {
      self.skip();
      Ok(tok.loc.wrap(id))
    } else {
      self.fail("an identifier", tok)
    }
  }

  fn maybe_long_id(&mut self) -> Result<Option<Long<StrRef>>> {
    let mut structures = Vec::new();
    loop {
      let tok = self.peek();
      if let Token::Ident(id, typ) = tok.val {
        self.skip();
        structures.push(tok.loc.wrap(id));
        if let IdentType::Symbolic = typ {
          break;
        }
        if let Token::Dot = self.peek().val {
          self.skip();
          continue;
        }
        break;
      }
      return if structures.is_empty() {
        Ok(None)
      } else {
        self.fail("an identifier", self.peek())
      };
    }
    let last = structures.pop().unwrap();
    structures.shrink_to_fit();
    Ok(Some(Long { structures, last }))
  }

  fn long_id(&mut self, allow_infix: bool) -> Result<Long<StrRef>> {
    let ret = match self.maybe_long_id()? {
      Some(x) => x,
      None => return self.fail("an identifier", self.peek()),
    };
    if !allow_infix && ret.structures.is_empty() && self.ops.contains_key(&ret.last.val) {
      Err(ret.last.loc.wrap(Error::InfixWithoutOp(ret.last.val)))
    } else {
      Ok(ret)
    }
  }

  fn long_alpha_num_id(&mut self) -> Result<Long<StrRef>> {
    let mut structures = Vec::new();
    loop {
      let tok = self.peek();
      if let Token::Ident(id, IdentType::AlphaNum) = tok.val {
        self.skip();
        structures.push(tok.loc.wrap(id));
        if let Token::Dot = self.peek().val {
          self.skip();
          continue;
        }
        break;
      }
      return self.fail("an identifier", self.peek());
    }
    let last = structures.pop().unwrap();
    Ok(Long { structures, last })
  }

  fn label(&mut self) -> Result<Located<Label>> {
    let tok = self.peek();
    self.skip();
    let ret = match tok.val {
      Token::DecInt(n, IsNumLab::Maybe) => Label::Num(n.try_into().unwrap()),
      Token::Ident(id, _) => Label::Vid(id),
      _ => return self.fail("a label", tok),
    };
    Ok(tok.loc.wrap(ret))
  }

  fn exp(&mut self) -> Result<Located<Exp<StrRef>>> {
    self.exp_prec(None)
  }

  fn exp_prec(&mut self, min_prec: Option<OpInfo>) -> Result<Located<Exp<StrRef>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let ret = match tok.val {
      Token::Raise => {
        self.skip();
        let e = self.exp()?;
        Exp::Raise(e.into())
      }
      Token::If => {
        self.skip();
        let e_cond = self.exp()?;
        self.eat(Token::Then)?;
        let e_then = self.exp()?;
        self.eat(Token::Else)?;
        let e_else = self.exp()?;
        Exp::If(e_cond.into(), e_then.into(), e_else.into())
      }
      Token::While => {
        self.skip();
        let e_cond = self.exp()?;
        self.eat(Token::Do)?;
        let e_body = self.exp()?;
        Exp::While(e_cond.into(), e_body.into())
      }
      Token::Case => {
        self.skip();
        let e_head = self.exp()?;
        self.eat(Token::Of)?;
        let cases = self.cases()?;
        Exp::Case(e_head.into(), cases)
      }
      Token::Fn => {
        self.skip();
        let cases = self.cases()?;
        Exp::Fn(cases)
      }
      _ => {
        let mut exp = self.at_exp()?;
        loop {
          let tok = self.peek();
          exp = exp.loc.wrap(match tok.val {
            Token::Ident(..) | Token::Equal => {
              let id = match tok.val {
                Token::Ident(id, _) => id,
                Token::Equal => StrRef::EQ,
                _ => unreachable!(),
              };
              self.skip();
              if self.peek().val == Token::Dot {
                self.i -= 1;
                let long = self.long_id(true)?;
                let rhs = exp.loc.wrap(Exp::LongVid(long));
                Exp::App(exp.into(), rhs.into())
              } else {
                match self.ops.get(&id) {
                  Some(&op_info) => {
                    if op_info.should_break(min_prec, tok.loc)? {
                      self.i -= 1;
                      break;
                    }
                    let rhs = self.exp_prec(Some(op_info))?;
                    Exp::InfixApp(exp.into(), tok.loc.wrap(id), rhs.into())
                  }
                  None => {
                    // not a LongVid because we just saw not Dot after this.
                    let rhs = exp.loc.wrap(Exp::LongVid(Long {
                      structures: Vec::new(),
                      last: tok.loc.wrap(id),
                    }));
                    Exp::App(exp.into(), rhs.into())
                  }
                }
              }
            }
            Token::Colon => {
              if min_prec.is_some() {
                break;
              }
              self.skip();
              let ty = self.ty()?;
              Exp::Typed(exp.into(), ty)
            }
            Token::Andalso => {
              if min_prec.is_some() {
                break;
              }
              self.skip();
              let rhs = self.exp()?;
              Exp::Andalso(exp.into(), rhs.into())
            }
            Token::Orelse => {
              if min_prec.is_some() {
                break;
              }
              self.skip();
              let rhs = self.exp()?;
              Exp::Orelse(exp.into(), rhs.into())
            }
            Token::Handle => {
              if min_prec.is_some() {
                break;
              }
              self.skip();
              Exp::Handle(exp.into(), self.cases()?)
            }
            _ => match self.maybe_at_exp()? {
              Some(rhs) => Exp::App(exp.into(), rhs.into()),
              None => break,
            },
          });
        }
        exp.val
      }
    };
    Ok(self.wrap(begin, ret))
  }

  fn cases(&mut self) -> Result<Cases<StrRef>> {
    let mut arms = Vec::new();
    loop {
      let pat = self.pat()?;
      self.eat(Token::BigArrow)?;
      let exp = self.exp()?;
      arms.push(Arm { pat, exp });
      if let Token::Bar = self.peek().val {
        self.skip();
      } else {
        break;
      }
    }
    arms.shrink_to_fit();
    Ok(Cases { arms })
  }

  fn maybe_dec(&mut self) -> Result<Option<Located<Dec<StrRef>>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let ret = match tok.val {
      Token::Val => {
        self.skip();
        let ty_vars = self.ty_var_seq()?;
        let mut val_binds = Vec::new();
        loop {
          let rec = if let Token::Rec = self.peek().val {
            self.skip();
            true
          } else {
            false
          };
          let pat = self.pat()?;
          self.eat(Token::Equal)?;
          let exp = self.exp()?;
          val_binds.push(ValBind { rec, pat, exp });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        val_binds.shrink_to_fit();
        Dec::Val(ty_vars, val_binds)
      }
      Token::Fun => {
        self.skip();
        let ty_vars = self.ty_var_seq()?;
        let mut cases = Vec::new();
        let mut binds = Vec::new();
        loop {
          cases.push(self.fval_bind_case()?);
          let tok = self.peek();
          if let Token::Bar = tok.val {
            self.skip();
            continue;
          }
          cases.shrink_to_fit();
          binds.push(FValBind { cases });
          if let Token::And = tok.val {
            self.skip();
            cases = Vec::new();
            continue;
          }
          break;
        }
        binds.shrink_to_fit();
        Dec::Fun(ty_vars, binds)
      }
      Token::Type => {
        self.skip();
        Dec::Type(self.ty_binds()?)
      }
      Token::Datatype => match self.datatype_dec(true)? {
        DatatypeDec::Binds(dat_binds) => {
          let ty_binds = if let Token::Withtype = self.peek().val {
            self.skip();
            self.ty_binds()?
          } else {
            Vec::new()
          };
          Dec::Datatype(dat_binds, ty_binds)
        }
        DatatypeDec::Copy(ty_con, long) => Dec::DatatypeCopy(ty_con, long),
      },
      Token::Abstype => {
        self.skip();
        let mut dat_binds = vec![self.dat_bind(true)?];
        while let Token::And = self.peek().val {
          self.skip();
          dat_binds.push(self.dat_bind(true)?);
        }
        let ty_binds = if let Token::Withtype = self.peek().val {
          self.skip();
          self.ty_binds()?
        } else {
          Vec::new()
        };
        self.eat(Token::With)?;
        let dec = self.dec()?;
        self.eat(Token::End)?;
        Dec::Abstype(dat_binds, ty_binds, dec.into())
      }
      Token::Exception => {
        self.skip();
        let mut ex_binds = Vec::new();
        loop {
          if let Token::Op = self.peek().val {
            self.skip();
          }
          let vid = self.ident()?;
          let inner = if let Token::Equal = self.peek().val {
            self.skip();
            if let Token::Op = self.peek().val {
              self.skip();
            }
            ExBindInner::Long(self.long_id(true)?)
          } else {
            ExBindInner::Ty(self.maybe_of_ty()?)
          };
          ex_binds.push(ExBind { vid, inner });
          if let Token::And = self.peek().val {
            self.skip();
          } else {
            break;
          }
        }
        ex_binds.shrink_to_fit();
        Dec::Exception(ex_binds)
      }
      Token::Local => {
        self.skip();
        let ops = self.ops.clone();
        let fst = self.dec()?;
        self.eat(Token::In)?;
        let snd = self.dec()?;
        self.eat(Token::End)?;
        self.ops = ops;
        Dec::Local(fst.into(), snd.into())
      }
      Token::Open => {
        self.skip();
        let mut str_ids = Vec::new();
        loop {
          str_ids.push(self.long_alpha_num_id()?);
          if let Token::Ident(..) = self.peek().val {
            continue;
          } else {
            break;
          }
        }
        str_ids.shrink_to_fit();
        Dec::Open(str_ids)
      }
      Token::Infix => {
        self.skip();
        let n = self.fixity_num()?;
        let idents = self.fixity_idents()?;
        for id in idents.iter() {
          self.ops.insert(id.val, OpInfo::left(n.val));
        }
        Dec::Infix(n, idents)
      }
      Token::Infixr => {
        self.skip();
        let n = self.fixity_num()?;
        let idents = self.fixity_idents()?;
        for id in idents.iter() {
          self.ops.insert(id.val, OpInfo::right(n.val));
        }
        Dec::Infixr(n, idents)
      }
      Token::Nonfix => {
        self.skip();
        let idents = self.fixity_idents()?;
        for id in idents.iter() {
          self.ops.remove(&id.val);
        }
        Dec::Nonfix(idents)
      }
      _ => return Ok(None),
    };
    Ok(Some(self.wrap(begin, ret)))
  }

  fn dec(&mut self) -> Result<Located<Dec<StrRef>>> {
    self.semicolon_seq(Self::maybe_dec, Dec::Seq)
  }

  fn fval_bind_case(&mut self) -> Result<FValBindCase<StrRef>> {
    let cur = self.i;
    let (vid, pats) = if let Ok((vid, pat)) = self.fval_bind_case_no_parens() {
      (vid, vec![pat])
    } else {
      // NOTE unbounded backtrack
      self.i = cur;
      let tok = self.peek();
      self.skip();
      let (vid, pat) = match tok.val {
        Token::Op => (self.ident()?, self.at_pat()?),
        Token::LRound => {
          let x = self.fval_bind_case_no_parens()?;
          self.eat(Token::RRound)?;
          x
        }
        Token::Ident(vid, _) => {
          if self.ops.contains_key(&vid) {
            return Err(tok.loc.wrap(Error::InfixWithoutOp(vid)));
          }
          (tok.loc.wrap(vid), self.at_pat()?)
        }
        _ => return self.fail("`op`, `(`, or an identifier", tok),
      };
      let mut pats = vec![pat];
      while let Some(pat) = self.maybe_at_pat()? {
        pats.push(pat);
      }
      (vid, pats)
    };
    let ret_ty = self.maybe_colon_ty()?;
    self.eat(Token::Equal)?;
    let body = self.exp()?;
    Ok(FValBindCase {
      vid,
      pats,
      ret_ty,
      body,
    })
  }

  fn fval_bind_case_no_parens(&mut self) -> Result<(Located<StrRef>, Located<Pat<StrRef>>)> {
    let fst = self.at_pat()?;
    let vid = self.ident()?;
    if !self.ops.contains_key(&vid.val) {
      return Err(vid.loc.wrap(Error::NotInfix(vid.val)));
    }
    let snd = self.at_pat()?;
    Ok((vid, fst.loc.wrap(Pat::Tuple(vec![fst, snd]))))
  }

  fn ty_binds(&mut self) -> Result<Vec<TyBind<StrRef>>> {
    let mut ret = Vec::new();
    loop {
      let ty_vars = self.ty_var_seq()?;
      let ty_con = self.ident()?;
      self.eat(Token::Equal)?;
      let ty = self.ty()?;
      ret.push(TyBind {
        ty_vars,
        ty_con,
        ty,
      });
      if let Token::And = self.peek().val {
        self.skip();
      } else {
        break;
      }
    }
    ret.shrink_to_fit();
    Ok(ret)
  }

  fn datatype_dec(&mut self, allow_op: bool) -> Result<DatatypeDec<StrRef>> {
    self.skip();
    let tok = self.peek();
    let dat_bind = if let Token::Ident(id, _) = tok.val {
      let ty_con = tok.loc.wrap(id);
      self.skip();
      self.eat(Token::Equal)?;
      if let Token::Datatype = self.peek().val {
        self.skip();
        let long = self.long_id(true)?;
        return Ok(DatatypeDec::Copy(ty_con, long));
      }
      let cons = self.con_binds(allow_op)?;
      DatBind {
        ty_vars: Vec::new(),
        ty_con,
        cons,
      }
    } else {
      self.dat_bind(allow_op)?
    };
    let mut dat_binds = vec![dat_bind];
    while let Token::And = self.peek().val {
      self.skip();
      dat_binds.push(self.dat_bind(allow_op)?);
    }
    Ok(DatatypeDec::Binds(dat_binds))
  }

  fn con_binds(&mut self, allow_op: bool) -> Result<Vec<ConBind<StrRef>>> {
    let mut ret = Vec::new();
    loop {
      if allow_op {
        if let Token::Op = self.peek().val {
          self.skip();
        }
      }
      let vid = self.ident()?;
      let ty = self.maybe_of_ty()?;
      ret.push(ConBind { vid, ty });
      if let Token::Bar = self.peek().val {
        self.skip();
      } else {
        break;
      }
    }
    ret.shrink_to_fit();
    Ok(ret)
  }

  fn dat_bind(&mut self, allow_op: bool) -> Result<DatBind<StrRef>> {
    let ty_vars = self.ty_var_seq()?;
    let ty_con = self.ident()?;
    self.eat(Token::Equal)?;
    let cons = self.con_binds(allow_op)?;
    Ok(DatBind {
      ty_vars,
      ty_con,
      cons,
    })
  }

  fn ty_var_seq(&mut self) -> Result<Vec<Located<TyVar<StrRef>>>> {
    let tok = self.peek();
    match tok.val {
      Token::TyVar(ty_var) => {
        self.skip();
        Ok(vec![tok.loc.wrap(ty_var)])
      }
      Token::LRound => {
        self.skip();
        let mut ret = Vec::new();
        if let Token::TyVar(..) = self.peek().val {
          // proceed.
        } else {
          // go back. this might be `fun (a f b) = ...` or `val (a, b) = ...` or something.
          self.i -= 1;
          return Ok(ret);
        }
        loop {
          let tok = self.peek();
          if let Token::TyVar(ty_var) = tok.val {
            self.skip();
            ret.push(tok.loc.wrap(ty_var));
          } else {
            return self.fail("a type variable", tok);
          }
          let tok = self.peek();
          self.skip();
          match tok.val {
            Token::RRound => break,
            Token::Comma => continue,
            _ => return self.fail("`)` or `,`", tok),
          }
        }
        ret.shrink_to_fit();
        Ok(ret)
      }
      _ => Ok(Vec::new()),
    }
  }

  fn maybe_at_pat(&mut self) -> Result<Option<Located<Pat<StrRef>>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let ret = match tok.val {
      Token::Underscore => {
        self.skip();
        Pat::Wildcard
      }
      Token::DecInt(n, _) => {
        self.skip();
        Pat::DecInt(n)
      }
      Token::HexInt(n) => {
        self.skip();
        Pat::HexInt(n)
      }
      Token::DecWord(n) => {
        self.skip();
        Pat::DecWord(n)
      }
      Token::HexWord(n) => {
        self.skip();
        Pat::HexWord(n)
      }
      Token::Real(..) => return Err(begin.wrap(Error::RealPat)),
      Token::String(s) => {
        self.skip();
        Pat::String(s)
      }
      Token::Char(c) => {
        self.skip();
        Pat::Char(c)
      }
      Token::Op => {
        self.skip();
        Pat::LongVid(self.long_id(true)?)
      }
      Token::LCurly => {
        self.skip();
        let mut rows = Vec::new();
        let mut rest_loc = None;
        if let Token::RCurly = self.peek().val {
          self.skip();
        } else {
          loop {
            let tok = self.peek();
            if let Token::DotDotDot = tok.val {
              self.skip();
              rest_loc = Some(tok.loc);
              if let Token::RCurly = self.peek().val {
                self.skip();
                break;
              }
              return self.fail("`}`", tok);
            }
            let lab = self.label()?;
            let tok = self.peek();
            let val = if let Token::Equal = tok.val {
              self.skip();
              self.pat()?
            } else {
              let vid = match lab.val {
                Label::Vid(x) => lab.loc.wrap(x),
                Label::Num(..) => return self.fail("`=`", tok),
              };
              let ty = self.maybe_colon_ty()?;
              let as_pat = self.maybe_as_pat()?;
              match as_pat {
                Some(as_pat) => vid
                  .loc
                  .span(as_pat.loc)
                  .wrap(Pat::As(vid, ty, as_pat.into())),
                None => {
                  let pat = vid.loc.wrap(Pat::LongVid(Long {
                    structures: Vec::new(),
                    last: vid,
                  }));
                  match ty {
                    None => pat,
                    Some(ty) => vid.loc.span(ty.loc).wrap(Pat::Typed(pat.into(), ty)),
                  }
                }
              }
            };
            rows.push(Row { lab, val });
            let tok = self.peek();
            self.skip();
            match tok.val {
              Token::RCurly => break,
              Token::Comma => continue,
              _ => return self.fail("`}` or `,`", tok),
            }
          }
        }
        rows.shrink_to_fit();
        Pat::Record(rows, rest_loc)
      }
      Token::LRound => {
        self.skip();
        let tok = self.peek();
        let mut pats = Vec::new();
        if let Token::RRound = tok.val {
          self.skip();
        } else {
          loop {
            pats.push(self.pat()?);
            let tok = self.peek();
            self.skip();
            match tok.val {
              Token::RRound => break,
              Token::Comma => continue,
              _ => return self.fail("`)` or `,`", tok),
            }
          }
        }
        if pats.len() == 1 {
          pats.pop().unwrap().val
        } else {
          pats.shrink_to_fit();
          Pat::Tuple(pats)
        }
      }
      Token::LSquare => {
        self.skip();
        let mut pats = Vec::new();
        if let Token::RSquare = self.peek().val {
          self.skip();
        } else {
          loop {
            pats.push(self.pat()?);
            let tok = self.peek();
            self.skip();
            match tok.val {
              Token::RSquare => break,
              Token::Comma => continue,
              _ => return self.fail("`]` or `,`", tok),
            }
          }
        }
        pats.shrink_to_fit();
        Pat::List(pats)
      }
      Token::Ident(..) => Pat::LongVid(self.long_id(false)?),
      _ => return Ok(None),
    };
    Ok(Some(self.wrap(begin, ret)))
  }

  fn at_pat(&mut self) -> Result<Located<Pat<StrRef>>> {
    match self.maybe_at_pat()? {
      Some(x) => Ok(x),
      None => self.fail("a pattern", self.peek()),
    }
  }

  fn pat(&mut self) -> Result<Located<Pat<StrRef>>> {
    self.pat_prec(None)
  }

  fn pat_prec(&mut self, min_prec: Option<OpInfo>) -> Result<Located<Pat<StrRef>>> {
    let mut ret = self.at_pat()?;
    if let Pat::LongVid(long_vid) = ret.val {
      let pat = self.pat_long_vid(ret.loc, long_vid)?;
      ret = self.wrap(ret.loc, pat);
    }
    loop {
      let loc = ret.loc;
      let tok = self.peek();
      let pat = match tok.val {
        Token::Colon => {
          if min_prec.is_some() {
            break;
          }
          self.skip();
          let ty = self.ty()?;
          Pat::Typed(ret.into(), ty)
        }
        Token::Ident(id, _) => {
          let op_info = match self.ops.get(&id) {
            Some(x) => *x,
            None => return Err(tok.loc.wrap(Error::NotInfix(id))),
          };
          if op_info.should_break(min_prec, tok.loc)? {
            break;
          }
          self.skip();
          let rhs = self.pat_prec(Some(op_info))?;
          Pat::InfixCtor(ret.into(), tok.loc.wrap(id), rhs.into())
        }
        _ => break,
      };
      ret = self.wrap(loc, pat);
    }
    Ok(ret)
  }

  fn pat_long_vid(&mut self, loc: Loc, long_vid: Long<StrRef>) -> Result<Pat<StrRef>> {
    if long_vid.structures.is_empty() {
      let ty = self.maybe_colon_ty()?;
      match self.maybe_as_pat()? {
        None => match ty {
          None => {}
          Some(ty) => return Ok(Pat::Typed(loc.wrap(Pat::LongVid(long_vid)).into(), ty)),
        },
        Some(as_pat) => {
          return Ok(Pat::As(long_vid.last, ty, as_pat.into()));
        }
      }
    }
    if let Token::Ident(id, _) = self.peek().val {
      if self.ops.get(&id).is_some() {
        // fall back out to pat_prec.
        return Ok(Pat::LongVid(long_vid));
      }
    }
    match self.maybe_at_pat()? {
      None => Ok(Pat::LongVid(long_vid)),
      Some(x) => Ok(Pat::Ctor(long_vid, x.into())),
    }
  }

  fn maybe_colon_ty(&mut self) -> Result<Option<Located<Ty<StrRef>>>> {
    if let Token::Colon = self.peek().val {
      self.skip();
      Ok(Some(self.ty()?))
    } else {
      Ok(None)
    }
  }

  fn maybe_as_pat(&mut self) -> Result<Option<Located<Pat<StrRef>>>> {
    if let Token::As = self.peek().val {
      self.skip();
      Ok(Some(self.pat()?))
    } else {
      Ok(None)
    }
  }

  fn ty(&mut self) -> Result<Located<Ty<StrRef>>> {
    self.ty_prec(TyPrec::Arrow)
  }

  fn ty_prec(&mut self, min_prec: TyPrec) -> Result<Located<Ty<StrRef>>> {
    let tok = self.peek();
    let begin = tok.loc;
    let mut ret = match tok.val {
      Token::TyVar(tv) => {
        self.skip();
        Ty::TyVar(tv)
      }
      Token::LCurly => {
        self.skip();
        let mut rows = Vec::new();
        if let Token::RCurly = self.peek().val {
          self.skip();
        } else {
          loop {
            let lab = self.label()?;
            self.eat(Token::Colon)?;
            let val = self.ty()?;
            rows.push(Row { lab, val });
            let tok = self.peek();
            self.skip();
            match tok.val {
              Token::RCurly => break,
              Token::Comma => continue,
              _ => return self.fail("`}` or `,`", tok),
            }
          }
        }
        rows.shrink_to_fit();
        Ty::Record(rows)
      }
      Token::LRound => {
        self.skip();
        let mut types = Vec::new();
        loop {
          types.push(self.ty()?);
          let tok = self.peek();
          self.skip();
          match tok.val {
            Token::RRound => break,
            Token::Comma => continue,
            _ => return self.fail("`(` or `,`", tok),
          }
        }
        let long_ty_con = self.maybe_long_id()?;
        types.shrink_to_fit();
        match (types.len(), long_ty_con) {
          (1, None) => types.pop().unwrap().val,
          (_, None) => return self.fail("an identifier", self.peek()),
          (_, Some(x)) => Ty::TyCon(types, x),
        }
      }
      Token::Ident(ref id, _) => {
        if *id == StrRef::STAR {
          // can't use self.fail here since the error message will say 'expected a type, found a
          // symbolic identifier' which is just confusing.
          return Err(tok.loc.wrap(Error::ExpectedButFound("a type", "`*`")));
        }
        let long_ty_con = self.long_id(true)?;
        Ty::TyCon(Vec::new(), long_ty_con)
      }
      _ => return self.fail("a type", tok),
    };
    loop {
      let tok = self.peek();
      match tok.val {
        Token::Arrow => {
          if TyPrec::Arrow < min_prec {
            break;
          }
          let lhs = self.wrap(begin, ret);
          self.skip();
          let rhs = self.ty_prec(TyPrec::Arrow)?;
          ret = Ty::Arrow(lhs.into(), rhs.into());
        }
        Token::Ident(ref id, _) => {
          if *id == StrRef::STAR {
            if TyPrec::Star < min_prec {
              break;
            }
            let mut types = vec![self.wrap(begin, ret)];
            self.skip();
            loop {
              types.push(self.ty_prec(TyPrec::App)?);
              if let Token::Ident(ref id, _) = self.peek().val {
                if *id == StrRef::STAR {
                  self.skip();
                  continue;
                }
              }
              break;
            }
            ret = Ty::Tuple(types);
          } else {
            let lhs = self.wrap(begin, ret);
            let long = self.long_id(true)?;
            ret = Ty::TyCon(vec![lhs], long);
          }
        }
        _ => break,
      }
    }
    Ok(self.wrap(begin, ret))
  }

  fn semicolon_seq<T, F, G>(&mut self, one: F, seq: G) -> Result<Located<T>>
  where
    F: Fn(&mut Self) -> Result<Option<Located<T>>>,
    G: FnOnce(Vec<Located<T>>) -> T,
  {
    let mut xs = Vec::new();
    while let Some(x) = one(self)? {
      xs.push(x);
      if let Token::Semicolon = self.peek().val {
        self.skip();
      }
    }
    xs.shrink_to_fit();
    let ret = match xs.len() {
      // NOTE we conjure up a 'fake' loc in the 0 case
      0 => self.peek().loc.wrap(seq(Vec::new())),
      1 => xs.pop().unwrap(),
      _ => xs
        .first()
        .unwrap()
        .loc
        .span(xs.last().unwrap().loc)
        .wrap(seq(xs)),
    };
    Ok(ret)
  }

  fn maybe_of_ty(&mut self) -> Result<Option<Located<Ty<StrRef>>>> {
    if let Token::Of = self.peek().val {
      self.skip();
      Ok(Some(self.ty()?))
    } else {
      Ok(None)
    }
  }

  fn fixity_num(&mut self) -> Result<Located<u32>> {
    let tok = self.peek();
    let loc = tok.loc;
    let ret = if let Token::DecInt(n, _) = tok.val {
      if n < 0 {
        return Err(loc.wrap(Error::NegativeFixity));
      }
      self.skip();
      n.try_into().unwrap()
    } else {
      0
    };
    Ok(loc.wrap(ret))
  }

  fn fixity_idents(&mut self) -> Result<Vec<Located<StrRef>>> {
    let mut ret = Vec::new();
    loop {
      let tok = self.peek();
      match tok.val {
        Token::Ident(id, _) => {
          self.skip();
          ret.push(tok.loc.wrap(id));
        }
        Token::Equal => {
          self.skip();
          ret.push(tok.loc.wrap(StrRef::EQ));
        }
        _ => break,
      }
    }
    if ret.is_empty() {
      self.fail("an identifier", self.peek())
    } else {
      ret.shrink_to_fit();
      Ok(ret)
    }
  }
}

enum DatatypeDec<I> {
  Binds(Vec<DatBind<I>>),
  Copy(Located<I>, Long<I>),
}

#[derive(Clone, Copy)]
struct OpInfo {
  num: u32,
  assoc: Assoc,
}

impl OpInfo {
  /// Returns a new OpInfo with left associativity.
  fn left(num: u32) -> Self {
    Self {
      num,
      assoc: Assoc::Left,
    }
  }

  /// Returns a new OpInfo with right associativity.
  fn right(num: u32) -> Self {
    Self {
      num,
      assoc: Assoc::Right,
    }
  }

  fn should_break(&self, min_prec: Option<Self>, loc: Loc) -> Result<bool> {
    match min_prec {
      None => Ok(false),
      Some(min_prec) => {
        if self.num == min_prec.num && self.assoc != min_prec.assoc {
          return Err(loc.wrap(Error::SameFixityDiffAssoc));
        }
        match min_prec.assoc {
          Assoc::Left => Ok(self.num <= min_prec.num),
          Assoc::Right => Ok(self.num < min_prec.num),
        }
      }
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Assoc {
  Left,
  Right,
}

#[test]
fn option_compare() {
  let none: Option<usize> = None;
  assert!(none < Some(3));
  assert!(Some(3) == Some(3));
  assert!(Some(3) < Some(5));
}
