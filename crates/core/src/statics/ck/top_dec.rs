//! Check top-level declarations.

use crate::ast::{SigExp, Spec, StrDec, StrExp, TopDec};
use crate::intern::StrRef;
use crate::loc::Located;
use crate::statics::ck::util::{env_ins, get_env};
use crate::statics::ck::{dec, ty};
use crate::statics::types::{
  Basis, Env, Error, FunEnv, Item, Result, Sig, SigEnv, State, StrEnv, SymTyInfo, Ty, TyEnv,
  TyInfo, TyScheme, ValEnv, ValInfo,
};

pub fn ck(bs: &mut Basis, st: &mut State, top_dec: &Located<TopDec<StrRef>>) -> Result<()> {
  match &top_dec.val {
    // SML Definition (87)
    TopDec::StrDec(str_dec) => {
      let env = ck_str_dec(bs, st, str_dec)?;
      bs.add_env(env);
    }
    // SML Definition (88)
    TopDec::SigDec(sig_binds) => {
      let mut sig_env = SigEnv::new();
      // SML Definition (66), SML Definition (67)
      for sig_bind in sig_binds {
        let env = ck_sig_exp(bs, st, &sig_bind.exp)?;
        // allow shadowing.
        sig_env.insert(sig_bind.id.val, env_to_sig(bs, env));
      }
      bs.add_sig_env(sig_env);
    }
    // SML Definition (85), SML Definition (89)
    TopDec::FunDec(fun_binds) => {
      let fun_env = FunEnv::new();
      // SML Definition (86)
      if let Some(fun_bind) = fun_binds.first() {
        return Err(fun_bind.fun_id.loc.wrap(Error::Todo("`functor`")));
      }
      bs.add_fun_env(fun_env);
    }
  }
  'outer: for (tv, (loc, overloads)) in std::mem::take(&mut st.overload) {
    for ty in overloads {
      let mut pre = st.subst.clone();
      if let Ok(()) = pre.unify(loc, &st.sym_tys, Ty::Var(tv), ty) {
        st.subst = pre;
        continue 'outer;
      }
    }
    return Err(loc.wrap(Error::NoSuitableOverload));
  }
  Ok(())
}

/// SML Definition (65)
fn env_to_sig(bs: &Basis, env: Env) -> Sig {
  let ty_names = env.ty_names().difference(&bs.ty_names).copied().collect();
  Sig { env, ty_names }
}

fn ck_str_exp(bs: &Basis, st: &mut State, str_exp: &Located<StrExp<StrRef>>) -> Result<Env> {
  match &str_exp.val {
    // SML Definition (50)
    StrExp::Struct(str_dec) => ck_str_dec(bs, st, str_dec),
    // SML Definition (51)
    StrExp::LongStrId(long) => match get_env(&bs.env, long)?.str_env.get(&long.last.val) {
      None => {
        let err = Error::Undefined(Item::Struct, long.last.val);
        Err(long.last.loc.wrap(err))
      }
      Some(env) => Ok(env.clone()),
    },
    // SML Definition (52), SML Definition (53)
    StrExp::Ascription(_, _, _) => Err(str_exp.loc.wrap(Error::Todo("signature ascription"))),
    // SML Definition (54)
    StrExp::FunctorApp(_, _) => Err(str_exp.loc.wrap(Error::Todo("functor application"))),
    // SML Definition (55)
    StrExp::Let(fst, snd) => {
      let env = ck_str_dec(bs, st, fst)?;
      let mut bs = bs.clone();
      bs.add_env(env);
      ck_str_exp(&bs, st, snd)
    }
  }
}

fn ck_str_dec(bs: &Basis, st: &mut State, str_dec: &Located<StrDec<StrRef>>) -> Result<Env> {
  match &str_dec.val {
    // SML Definition (56)
    StrDec::Dec(dec) => dec::ck(&bs.to_cx(), st, dec),
    // SML Definition (57)
    StrDec::Structure(str_binds) => {
      let mut bs = bs.clone();
      let mut str_env = StrEnv::new();
      // SML Definition (61)
      for str_bind in str_binds {
        let env = ck_str_exp(&bs, st, &str_bind.exp)?;
        bs.ty_names.extend(env.ty_names());
        // allow shadowing.
        str_env.insert(str_bind.id.val, env);
      }
      Ok(str_env.into())
    }
    // SML Definition (58)
    StrDec::Local(fst, snd) => {
      let env = ck_str_dec(bs, st, fst)?;
      let mut bs = bs.clone();
      bs.add_env(env);
      ck_str_dec(&bs, st, snd)
    }
    // SML Definition (59), SML Definition (60)
    StrDec::Seq(str_decs) => {
      // TODO clone in loop - expensive?
      let mut bs = bs.clone();
      let mut ret = Env::default();
      for str_dec in str_decs {
        bs.add_env(ret.clone());
        ret.extend(ck_str_dec(&bs, st, str_dec)?);
      }
      Ok(ret)
    }
  }
}

fn ck_sig_exp(bs: &Basis, st: &mut State, sig_exp: &Located<SigExp<StrRef>>) -> Result<Env> {
  match &sig_exp.val {
    // SML Definition (62)
    SigExp::Sig(spec) => ck_spec(bs, st, spec),
    // SML Definition (63)
    SigExp::SigId(sig_id) => match bs.sig_env.get(&sig_id.val) {
      None => {
        let err = Error::Undefined(Item::Sig, sig_id.val);
        Err(sig_id.loc.wrap(err))
      }
      Some(sig) => {
        if sig.ty_names.is_disjoint(&bs.ty_names) {
          Ok(sig.env.clone())
        } else {
          // TODO rename the type names?
          Err(sig_exp.loc.wrap(Error::Todo("type name set intersection")))
        }
      }
    },
    // SML Definition (64)
    SigExp::Where(_, _, _, _) => Err(sig_exp.loc.wrap(Error::Todo("`where`"))),
  }
}

fn ck_spec(bs: &Basis, st: &mut State, spec: &Located<Spec<StrRef>>) -> Result<Env> {
  match &spec.val {
    // SML Definition (68)
    Spec::Val(val_descs) => {
      let cx = bs.to_cx();
      let mut val_env = ValEnv::new();
      // SML Definition (79)
      for val_desc in val_descs {
        let ty = ty::ck(&cx, &st.sym_tys, &val_desc.ty)?;
        // TODO generalize? closure?
        env_ins(&mut val_env, val_desc.vid, ValInfo::val(TyScheme::mono(ty)))?;
      }
      Ok(val_env.into())
    }
    // SML Definition (69), SML Definition (70)
    Spec::Type(ty_descs, equality) => {
      let mut ty_env = TyEnv::default();
      // SML Definition (80)
      for ty_desc in ty_descs {
        if let Some(tv) = ty_desc.ty_vars.first() {
          return Err(tv.loc.wrap(Error::Todo("type variables")));
        }
        let sym = st.new_sym(ty_desc.ty_con);
        // TODO equality check
        env_ins(&mut ty_env.inner, ty_desc.ty_con, TyInfo::Sym(sym))?;
        st.sym_tys.insert(
          sym,
          SymTyInfo {
            ty_fcn: TyScheme::mono(Ty::Ctor(vec![], sym)),
            val_env: ValEnv::new(),
            equality: *equality,
          },
        );
      }
      Ok(ty_env.into())
    }
    // SML Definition (71)
    Spec::Datatype(dat_binds) => dec::ck_dat_binds(bs.to_cx(), st, dat_binds),
    // SML Definition (72)
    Spec::DatatypeCopy(ty_con, long) => dec::ck_dat_copy(&bs.to_cx(), &st.sym_tys, *ty_con, long),
    // SML Definition (73)
    Spec::Exception(ex_descs) => {
      let cx = bs.to_cx();
      let mut val_env = ValEnv::new();
      // SML Definition (83)
      for ex_desc in ex_descs {
        let val_info = match &ex_desc.ty {
          None => ValInfo::exn(),
          Some(ty) => ValInfo::exn_fn(ty::ck(&cx, &st.sym_tys, ty)?),
        };
        env_ins(&mut val_env, ex_desc.vid, val_info)?;
      }
      Ok(val_env.into())
    }
    // SML Definition (74)
    Spec::Structure(str_descs) => {
      let mut bs = bs.clone();
      let mut str_env = StrEnv::new();
      // SML Definition (84)
      for str_desc in str_descs {
        let env = ck_sig_exp(&bs, st, &str_desc.exp)?;
        bs.ty_names.extend(env.ty_names());
        // allow shadowing.
        str_env.insert(str_desc.str_id.val, env);
      }
      Ok(str_env.into())
    }
    // SML Definition (75)
    Spec::Include(sig_exp) => ck_sig_exp(bs, st, sig_exp),
    // SML Definition (76), SML Definition (77)
    Spec::Seq(specs) => {
      let mut ret = Env::default();
      for spec in specs {
        let env = ck_spec(bs, st, spec)?;
        ret.maybe_extend(env, spec.loc)?;
      }
      Ok(ret)
    }
    // SML Definition (78)
    Spec::Sharing(_, _) => Err(spec.loc.wrap(Error::Todo("`sharing`"))),
  }
}
