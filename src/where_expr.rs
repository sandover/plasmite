//! Purpose: Compile and evaluate `plasmite peek --where` expressions against message JSON.
//! Exports: `WherePredicate`, `compile_where_predicates`, `matches_all_where`.
//! Role: Small adapter around `jaq-core` to support boolean filtering without shelling out to `jq`.
//! Invariants: Parse/compile failures are usage errors; runtime eval errors count as "no match".
//! Invariants: Each `--where` must yield only booleans (otherwise: usage error).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Error as JaqError, Native, RcIter};
use serde_json::Value;

use plasmite::core::error::{Error, ErrorKind};

#[derive(Clone)]
pub struct WherePredicate {
    expr: String,
    filter: jaq_core::Filter<Native<JaqValue>>,
}

impl fmt::Debug for WherePredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WherePredicate")
            .field("expr", &self.expr)
            .finish()
    }
}

impl WherePredicate {
    pub fn matches(&self, input: &Value) -> Result<bool, Error> {
        let input = JaqValue::from_json(input);
        let inputs = RcIter::new(core::iter::empty::<Result<JaqValue, String>>());
        let out = self.filter.run((Ctx::new([], &inputs), input));

        let mut any_true = false;
        for item in out {
            match item {
                Ok(JaqValue::Bool(true)) => any_true = true,
                Ok(JaqValue::Bool(false)) => {}
                Ok(other) => {
                    return Err(Error::new(ErrorKind::Usage)
                        .with_message("--where expression must yield booleans")
                        .with_hint(format!(
                            "Expression `{}` yielded non-boolean value: {other}",
                            self.expr
                        )));
                }
                Err(_runtime_err) => {
                    // Spec: runtime errors (missing fields, type mismatches) evaluate to false.
                    return Ok(false);
                }
            }
        }

        Ok(any_true)
    }
}

pub fn compile_where_predicates(exprs: &[String]) -> Result<Vec<WherePredicate>, Error> {
    exprs
        .iter()
        .map(|expr| compile_where_predicate(expr))
        .collect()
}

pub fn matches_all_where(predicates: &[WherePredicate], input: &Value) -> Result<bool, Error> {
    for predicate in predicates.iter() {
        if !predicate.matches(input)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn compile_where_predicate(expr: &str) -> Result<WherePredicate, Error> {
    let arena = Arena::default();
    let loader = Loader::new(std::iter::empty());

    let program = File {
        code: expr,
        path: (),
    };
    let modules = loader
        .load(&arena, program)
        .map_err(|errs| where_compile_error(expr, errs))?;

    let filter = Compiler::default()
        .with_funs(jaq_std::base_funs::<JaqValue>())
        .compile(modules)
        .map_err(|errs| where_compile_error(expr, errs))?;

    Ok(WherePredicate {
        expr: expr.to_string(),
        filter,
    })
}

fn where_compile_error<E: fmt::Debug>(expr: &str, err: E) -> Error {
    Error::new(ErrorKind::Usage)
        .with_message("invalid --where expression")
        .with_hint(format!(
            "Failed to parse/compile `{expr}`.\nDetails: {err:?}\nExample: --where '.data.kind == \"ping\"'"
        ))
}

#[derive(Clone, Debug)]
pub enum JaqValue {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JaqValue>),
    Obj(BTreeMap<String, JaqValue>),
}

impl JaqValue {
    fn kind_rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::Num(_) => 2,
            Self::Str(_) => 3,
            Self::Arr(_) => 4,
            Self::Obj(_) => 5,
        }
    }

    fn as_f64_opt(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn as_str_opt(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn from_json(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Number(n) => Self::Num(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => Self::Str(s.clone()),
            Value::Array(a) => Self::Arr(a.iter().map(Self::from_json).collect()),
            Value::Object(o) => Self::Obj(
                o.iter()
                    .map(|(k, v)| (k.clone(), Self::from_json(v)))
                    .collect(),
            ),
        }
    }
}

impl fmt::Display for JaqValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Num(n) => write!(f, "{n}"),
            Self::Str(s) => match serde_json::to_string(s) {
                Ok(encoded) => write!(f, "{encoded}"),
                Err(_) => write!(f, "\"<invalid string>\""),
            },
            Self::Arr(a) => {
                write!(f, "[")?;
                for (idx, item) in a.iter().enumerate() {
                    if idx > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Self::Obj(o) => {
                write!(f, "{{")?;
                for (idx, (k, v)) in o.iter().enumerate() {
                    if idx > 0 {
                        write!(f, ",")?;
                    }
                    let k = serde_json::to_string(k).unwrap_or_else(|_| "\"<key>\"".to_string());
                    write!(f, "{k}:{v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl From<bool> for JaqValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<isize> for JaqValue {
    fn from(value: isize) -> Self {
        Self::Num(value as f64)
    }
}

impl From<f64> for JaqValue {
    fn from(value: f64) -> Self {
        Self::Num(value)
    }
}

impl From<String> for JaqValue {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl FromIterator<Self> for JaqValue {
    fn from_iter<T: IntoIterator<Item = Self>>(iter: T) -> Self {
        Self::Arr(iter.into_iter().collect())
    }
}

impl PartialEq for JaqValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Num(a), Self::Num(b)) => a.to_bits() == b.to_bits(),
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::Arr(a), Self::Arr(b)) => a == b,
            (Self::Obj(a), Self::Obj(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for JaqValue {}

impl PartialOrd for JaqValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JaqValue {
    fn cmp(&self, other: &Self) -> Ordering {
        let ka = self.kind_rank();
        let kb = other.kind_rank();
        if ka != kb {
            return ka.cmp(&kb);
        }
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Num(a), Self::Num(b)) => a.total_cmp(b),
            (Self::Str(a), Self::Str(b)) => a.cmp(b),
            (Self::Arr(a), Self::Arr(b)) => a.cmp(b),
            (Self::Obj(a), Self::Obj(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

impl std::ops::Add for JaqValue {
    type Output = Result<Self, JaqError<Self>>;

    fn add(self, rhs: Self) -> Self::Output {
        use jaq_core::ops::Math;
        match (self, rhs) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(a + b)),
            (Self::Str(a), Self::Str(b)) => Ok(Self::Str(format!("{a}{b}"))),
            (Self::Arr(mut a), Self::Arr(b)) => {
                a.extend(b);
                Ok(Self::Arr(a))
            }
            (l, r) => Err(JaqError::math(l, Math::Add, r)),
        }
    }
}

impl std::ops::Sub for JaqValue {
    type Output = Result<Self, JaqError<Self>>;

    fn sub(self, rhs: Self) -> Self::Output {
        use jaq_core::ops::Math;
        match (self, rhs) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(a - b)),
            (l, r) => Err(JaqError::math(l, Math::Sub, r)),
        }
    }
}

impl std::ops::Mul for JaqValue {
    type Output = Result<Self, JaqError<Self>>;

    fn mul(self, rhs: Self) -> Self::Output {
        use jaq_core::ops::Math;
        match (self, rhs) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(a * b)),
            (l, r) => Err(JaqError::math(l, Math::Mul, r)),
        }
    }
}

impl std::ops::Div for JaqValue {
    type Output = Result<Self, JaqError<Self>>;

    fn div(self, rhs: Self) -> Self::Output {
        use jaq_core::ops::Math;
        match (self, rhs) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(a / b)),
            (l, r) => Err(JaqError::math(l, Math::Div, r)),
        }
    }
}

impl std::ops::Rem for JaqValue {
    type Output = Result<Self, JaqError<Self>>;

    fn rem(self, rhs: Self) -> Self::Output {
        use jaq_core::ops::Math;
        match (self, rhs) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(a % b)),
            (l, r) => Err(JaqError::math(l, Math::Rem, r)),
        }
    }
}

impl std::ops::Neg for JaqValue {
    type Output = Result<Self, JaqError<Self>>;

    fn neg(self) -> Self::Output {
        match self {
            Self::Num(a) => Ok(Self::Num(-a)),
            other => Err(JaqError::typ(other, "number")),
        }
    }
}

impl jaq_core::ValT for JaqValue {
    fn from_num(n: &str) -> Result<Self, JaqError<Self>> {
        let parsed = n.parse::<f64>().map_err(JaqError::str)?;
        Ok(Self::Num(parsed))
    }

    fn from_map<I: IntoIterator<Item = (Self, Self)>>(iter: I) -> Result<Self, JaqError<Self>> {
        let mut map = BTreeMap::new();
        for (k, v) in iter {
            let Some(key) = k.as_str_opt() else {
                return Err(JaqError::typ(k, "string"));
            };
            map.insert(key.to_string(), v);
        }
        Ok(Self::Obj(map))
    }

    fn values(self) -> Box<dyn Iterator<Item = Result<Self, JaqError<Self>>>> {
        match self {
            Self::Arr(values) => Box::new(values.into_iter().map(Ok)),
            Self::Obj(values) => Box::new(values.into_values().map(Ok)),
            other => Box::new(std::iter::once(Err(JaqError::typ(other, "iterable")))),
        }
    }

    fn index(self, index: &Self) -> Result<Self, JaqError<Self>> {
        match (self, index) {
            (Self::Obj(mut obj), Self::Str(key)) => obj
                .remove(key)
                .ok_or_else(|| JaqError::index(Self::Obj(obj), Self::Str(key.clone()))),
            (Self::Arr(arr), Self::Num(n)) => {
                if !n.is_finite() || n.fract() != 0.0 {
                    return Err(JaqError::typ(Self::Num(*n), "integer"));
                }
                let idx = *n as isize;
                let len = arr.len() as isize;
                let idx = if idx < 0 { len + idx } else { idx };
                let idx = usize::try_from(idx).map_err(JaqError::str)?;
                arr.get(idx)
                    .cloned()
                    .ok_or_else(|| JaqError::index(Self::Arr(arr), Self::Num(*n)))
            }
            (l, r) => Err(JaqError::index(l, r.clone())),
        }
    }

    fn range(self, range: jaq_core::val::Range<&Self>) -> Result<Self, JaqError<Self>> {
        let to_index = |v: &Self| -> Result<isize, JaqError<Self>> {
            match v {
                Self::Num(n) if n.is_finite() && n.fract() == 0.0 => Ok(*n as isize),
                other => Err(JaqError::typ(other.clone(), "integer")),
            }
        };
        match self {
            Self::Arr(arr) => {
                let len = arr.len() as isize;
                let start = range.start.map(to_index).transpose()?.unwrap_or(0);
                let end = range.end.map(to_index).transpose()?.unwrap_or(len);
                let norm = |idx: isize| if idx < 0 { len + idx } else { idx };
                let start = norm(start).clamp(0, len) as usize;
                let end = norm(end).clamp(0, len) as usize;
                let slice = if end >= start {
                    arr[start..end].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Self::Arr(slice))
            }
            other => Err(JaqError::typ(other, "array")),
        }
    }

    fn map_values<'a, I: Iterator<Item = jaq_core::ValX<'a, Self>>>(
        self,
        opt: jaq_core::path::Opt,
        f: impl Fn(Self) -> I,
    ) -> jaq_core::ValX<'a, Self> {
        match self {
            Self::Arr(values) => {
                let mut out = Vec::with_capacity(values.len());
                for value in values {
                    let mut iter = f(value);
                    match iter.next() {
                        Some(Ok(v)) => out.push(v),
                        Some(Err(e)) => return Err(e),
                        None => out.push(Self::Null),
                    }
                }
                Ok(Self::Arr(out))
            }
            Self::Obj(values) => {
                let mut out = BTreeMap::new();
                for (k, v) in values {
                    let mut iter = f(v);
                    match iter.next() {
                        Some(Ok(v)) => {
                            out.insert(k, v);
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            out.insert(k, Self::Null);
                        }
                    }
                }
                Ok(Self::Obj(out))
            }
            other => match opt {
                jaq_core::path::Opt::Optional => Ok(other),
                jaq_core::path::Opt::Essential => Err(JaqError::typ(other, "iterable").into()),
            },
        }
    }

    fn map_index<'a, I: Iterator<Item = jaq_core::ValX<'a, Self>>>(
        self,
        index: &Self,
        opt: jaq_core::path::Opt,
        f: impl Fn(Self) -> I,
    ) -> jaq_core::ValX<'a, Self> {
        match self {
            Self::Obj(mut obj) => {
                let Some(key) = index.as_str_opt() else {
                    return Err(JaqError::typ(index.clone(), "string").into());
                };
                match obj.remove(key) {
                    Some(value) => {
                        let mut iter = f(value);
                        let next = match iter.next() {
                            Some(Ok(v)) => v,
                            Some(Err(e)) => return Err(e),
                            None => Self::Null,
                        };
                        obj.insert(key.to_string(), next);
                        Ok(Self::Obj(obj))
                    }
                    None => match opt {
                        jaq_core::path::Opt::Optional => Ok(Self::Obj(obj)),
                        jaq_core::path::Opt::Essential => {
                            Err(JaqError::index(Self::Obj(obj), index.clone()).into())
                        }
                    },
                }
            }
            other => match opt {
                jaq_core::path::Opt::Optional => Ok(other),
                jaq_core::path::Opt::Essential => Err(JaqError::index(other, index.clone()).into()),
            },
        }
    }

    fn map_range<'a, I: Iterator<Item = jaq_core::ValX<'a, Self>>>(
        self,
        range: jaq_core::val::Range<&Self>,
        opt: jaq_core::path::Opt,
        f: impl Fn(Self) -> I,
    ) -> jaq_core::ValX<'a, Self> {
        match self {
            Self::Arr(arr) => {
                let slice = Self::Arr(arr).range(range)?;
                let mut iter = f(slice);
                match iter.next() {
                    Some(Ok(v)) => Ok(v),
                    Some(Err(e)) => Err(e),
                    None => Ok(Self::Null),
                }
            }
            other => match opt {
                jaq_core::path::Opt::Optional => Ok(other),
                jaq_core::path::Opt::Essential => Err(JaqError::typ(other, "array").into()),
            },
        }
    }

    fn as_bool(&self) -> bool {
        !matches!(self, Self::Null | Self::Bool(false))
    }

    fn as_str(&self) -> Option<&str> {
        self.as_str_opt()
    }
}

impl jaq_std::ValT for JaqValue {
    fn into_seq<S: FromIterator<Self>>(self) -> Result<S, Self> {
        match self {
            Self::Arr(values) => Ok(values.into_iter().collect()),
            other => Err(other),
        }
    }

    fn as_isize(&self) -> Option<isize> {
        let num = self.as_f64_opt()?;
        if !num.is_finite() || num.fract() != 0.0 {
            return None;
        }
        let cast = num as isize;
        if (cast as f64).to_bits() == num.to_bits() {
            Some(cast)
        } else {
            None
        }
    }

    fn as_f64(&self) -> Result<f64, JaqError<Self>> {
        self.as_f64_opt()
            .ok_or_else(|| JaqError::typ(self.clone(), "number"))
    }
}

#[cfg(test)]
mod tests {
    use super::{compile_where_predicates, matches_all_where};
    use serde_json::json;

    #[test]
    fn where_matches_simple_equality() {
        let preds = compile_where_predicates(&[r#".data.x == 1"#.to_string()]).unwrap();
        let msg = json!({"seq":1,"time":"t","meta":{"descrips":[]},"data":{"x":1}});
        assert!(matches_all_where(&preds, &msg).unwrap());
    }

    #[test]
    fn where_runtime_error_is_false() {
        let preds = compile_where_predicates(&[r#".data.missing == 1"#.to_string()]).unwrap();
        let msg = json!({"seq":1,"time":"t","meta":{"descrips":[]},"data":{"x":1}});
        assert!(!matches_all_where(&preds, &msg).unwrap());
    }

    #[test]
    fn where_non_boolean_output_is_usage_error() {
        let preds = compile_where_predicates(&[r#".data"#.to_string()]).unwrap();
        let msg = json!({"seq":1,"time":"t","meta":{"descrips":[]},"data":{"x":1}});
        assert!(matches_all_where(&preds, &msg).is_err());
    }

    #[test]
    fn where_any_true_across_multiple_outputs() {
        let preds =
            compile_where_predicates(&[r#".meta.descrips[]? == "ping""#.to_string()]).unwrap();
        let msg = json!({"seq":1,"time":"t","meta":{"descrips":["foo","ping"]},"data":{}});
        assert!(matches_all_where(&preds, &msg).unwrap());
    }
}
