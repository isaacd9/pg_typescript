use pgrx::{FromDatum, IntoDatum, pg_sys};
use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

/// Convert a Postgres datum to a `serde_json::Value` for passing into JS.
///
/// # Safety
/// `datum` must be a valid datum for the given `type_oid`, or the call
/// must have `isnull == true`.
pub unsafe fn datum_to_json(
    datum: pg_sys::Datum,
    isnull: bool,
    type_oid: pg_sys::Oid,
) -> Value {
    if isnull {
        return Value::Null;
    }

    unsafe {
        match type_oid {
            pg_sys::INT2OID => {
                let v = i16::from_datum(datum, false).unwrap_or(0);
                Value::Number(v.into())
            }
            pg_sys::INT4OID => {
                let v = i32::from_datum(datum, false).unwrap_or(0);
                Value::Number(v.into())
            }
            pg_sys::INT8OID => {
                let v = i64::from_datum(datum, false).unwrap_or(0);
                Value::Number(v.into())
            }
            pg_sys::FLOAT4OID => {
                let v = f32::from_datum(datum, false).unwrap_or(0.0);
                serde_json::Number::from_f64(v as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
            pg_sys::FLOAT8OID => {
                let v = f64::from_datum(datum, false).unwrap_or(0.0);
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
            pg_sys::BOOLOID => {
                let v = bool::from_datum(datum, false).unwrap_or(false);
                Value::Bool(v)
            }
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NAMEOID => {
                let v = String::from_datum(datum, false).unwrap_or_default();
                Value::String(v)
            }
            pg_sys::JSONOID | pg_sys::JSONBOID => {
                // pgrx::JsonB implements FromDatum and contains a serde_json::Value.
                let v: Option<pgrx::JsonB> = pgrx::JsonB::from_datum(datum, false);
                v.map(|j| j.0).unwrap_or(Value::Null)
            }
            _ => {
                // Fall back: try to cast to text via PostgreSQL's output function.
                let cstr = output_fn_call(datum, type_oid);
                Value::String(cstr)
            }
        }
    }
}

/// Convert a `serde_json::Value` back to a Postgres Datum.
///
/// Returns `(Datum, isnull)`.  The caller must set `fcinfo.isnull` if isnull is true.
pub fn json_to_datum(value: &Value, type_oid: pg_sys::Oid) -> (pg_sys::Datum, bool) {
    match value {
        Value::Null => (pg_sys::Datum::from(0usize), true),

        Value::Bool(b) => match type_oid {
            pg_sys::BOOLOID => ((*b).into_datum().unwrap(), false),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => {
                (b.to_string().into_datum().unwrap(), false)
            }
            _ => ((*b as i32).into_datum().unwrap(), false),
        },

        Value::Number(n) => match type_oid {
            pg_sys::INT2OID => {
                let v = n.as_i64().unwrap_or(0) as i16;
                (v.into_datum().unwrap(), false)
            }
            pg_sys::INT4OID => {
                let v = n.as_i64().unwrap_or(0) as i32;
                (v.into_datum().unwrap(), false)
            }
            pg_sys::INT8OID => {
                let v = n.as_i64().unwrap_or(0);
                (v.into_datum().unwrap(), false)
            }
            pg_sys::FLOAT4OID => {
                let v = n.as_f64().unwrap_or(0.0) as f32;
                (v.into_datum().unwrap(), false)
            }
            pg_sys::FLOAT8OID => {
                let v = n.as_f64().unwrap_or(0.0);
                (v.into_datum().unwrap(), false)
            }
            pg_sys::TEXTOID | pg_sys::VARCHAROID => {
                (n.to_string().into_datum().unwrap(), false)
            }
            pg_sys::BOOLOID => {
                let v = n.as_f64().unwrap_or(0.0) != 0.0;
                (v.into_datum().unwrap(), false)
            }
            _ => {
                // Generic: stringify and input via Postgres.
                let s = n.to_string();
                (input_fn_call(&s, type_oid), false)
            }
        },

        Value::String(s) => match type_oid {
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NAMEOID => {
                (s.as_str().into_datum().unwrap(), false)
            }
            pg_sys::INT4OID => {
                let v: i32 = s.parse().unwrap_or(0);
                (v.into_datum().unwrap(), false)
            }
            pg_sys::INT8OID => {
                let v: i64 = s.parse().unwrap_or(0);
                (v.into_datum().unwrap(), false)
            }
            pg_sys::FLOAT8OID => {
                let v: f64 = s.parse().unwrap_or(0.0);
                (v.into_datum().unwrap(), false)
            }
            pg_sys::BOOLOID => {
                let v = matches!(s.to_lowercase().as_str(), "true" | "1" | "yes" | "on");
                (v.into_datum().unwrap(), false)
            }
            pg_sys::JSONOID | pg_sys::JSONBOID => {
                // Assume the string is already valid JSON.
                let jb = pgrx::JsonB(
                    serde_json::from_str(s).unwrap_or(Value::String(s.clone())),
                );
                (jb.into_datum().unwrap(), false)
            }
            _ => (input_fn_call(s, type_oid), false),
        },

        Value::Array(_) | Value::Object(_) => match type_oid {
            pg_sys::JSONOID | pg_sys::JSONBOID => {
                let jb = pgrx::JsonB(value.clone());
                (jb.into_datum().unwrap(), false)
            }
            pg_sys::TEXTOID | pg_sys::VARCHAROID => {
                let s = value.to_string();
                (s.into_datum().unwrap(), false)
            }
            _ => {
                let s = value.to_string();
                (input_fn_call(&s, type_oid), false)
            }
        },
    }
}

// ---------------------------------------------------------------------------
// DeserializeSeed for direct V8 → Datum conversion (no JSON intermediary)
// ---------------------------------------------------------------------------

/// A [`DeserializeSeed`] that converts a deserialized V8 value directly into a
/// Postgres [`pg_sys::Datum`], using the supplied OID for type dispatch.
///
/// Returns `(Datum, isnull)`.  Delegates to [`json_to_datum`] for all cases so
/// the OID dispatch logic lives in exactly one place.
pub struct PgDatumSeed {
    pub oid: pg_sys::Oid,
}

impl<'de> DeserializeSeed<'de> for PgDatumSeed {
    type Value = (pg_sys::Datum, bool);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(PgDatumVisitor { oid: self.oid })
    }
}

struct PgDatumVisitor {
    oid: pg_sys::Oid,
}

impl<'de> Visitor<'de> for PgDatumVisitor {
    type Value = (pg_sys::Datum, bool);

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a value convertible to Postgres datum (OID {:?})", self.oid)
    }

    // JS null / undefined
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok((pg_sys::Datum::from(0usize), true))
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok((pg_sys::Datum::from(0usize), true))
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(json_to_datum(&Value::Bool(v), self.oid))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(json_to_datum(&Value::Number(v.into()), self.oid))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(json_to_datum(&Value::Number(serde_json::Number::from(v)), self.oid))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        let n = serde_json::Number::from_f64(v).unwrap_or_else(|| serde_json::Number::from(0));
        Ok(json_to_datum(&Value::Number(n), self.oid))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(json_to_datum(&Value::String(v.to_owned()), self.oid))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(json_to_datum(&Value::String(v), self.oid))
    }

    // Objects (e.g. JSONB return type) — collect into a serde_json::Value then convert.
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        let value = Value::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        Ok(json_to_datum(&value, self.oid))
    }
    // Arrays / JSONB arrays.
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        let value = Value::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
        Ok(json_to_datum(&value, self.oid))
    }
}

/// Call the type's output function to convert a datum to a string.
unsafe fn output_fn_call(datum: pg_sys::Datum, type_oid: pg_sys::Oid) -> String {
    unsafe {
        let mut output_fn: pg_sys::Oid = pg_sys::InvalidOid;
        let mut is_varlena: bool = false;
        pg_sys::getTypeOutputInfo(type_oid, &mut output_fn, &mut is_varlena);
        let cstr = pg_sys::OidOutputFunctionCall(output_fn, datum);
        let result = std::ffi::CStr::from_ptr(cstr)
            .to_string_lossy()
            .to_string();
        pg_sys::pfree(cstr.cast());
        result
    }
}

/// Call the type's input function to parse a string into a Datum.
fn input_fn_call(s: &str, type_oid: pg_sys::Oid) -> pg_sys::Datum {
    unsafe {
        let mut input_fn: pg_sys::Oid = pg_sys::InvalidOid;
        let mut ioparam: pg_sys::Oid = pg_sys::InvalidOid;
        pg_sys::getTypeInputInfo(type_oid, &mut input_fn, &mut ioparam);
        let cstr = std::ffi::CString::new(s).expect("NUL in value");
        pg_sys::OidInputFunctionCall(input_fn, cstr.as_ptr().cast_mut(), ioparam, -1)
    }
}
