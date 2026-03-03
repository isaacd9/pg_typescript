use pgrx::{pg_sys, FromDatum, IntoDatum};
use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Serialize: direct Datum → V8 value (no JSON intermediary)
// ---------------------------------------------------------------------------

/// A wrapper that serializes a Postgres datum directly into a V8 value via
/// `serde_v8::to_v8`, bypassing any `serde_json::Value` intermediary.
pub struct PgDatum {
    pub datum: pg_sys::Datum,
    pub isnull: bool,
    pub oid: pg_sys::Oid,
}

impl Serialize for PgDatum {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.isnull {
            return s.serialize_none();
        }
        // SAFETY: caller guarantees datum is valid for this oid.
        unsafe {
            match self.oid {
                pg_sys::INT2OID => s.serialize_i16(i16::from_datum(self.datum, false).unwrap_or(0)),
                pg_sys::INT4OID => s.serialize_i32(i32::from_datum(self.datum, false).unwrap_or(0)),
                pg_sys::INT8OID => s.serialize_i64(i64::from_datum(self.datum, false).unwrap_or(0)),
                pg_sys::FLOAT4OID => {
                    s.serialize_f32(f32::from_datum(self.datum, false).unwrap_or(0.0))
                }
                pg_sys::FLOAT8OID => {
                    s.serialize_f64(f64::from_datum(self.datum, false).unwrap_or(0.0))
                }
                pg_sys::BOOLOID => {
                    s.serialize_bool(bool::from_datum(self.datum, false).unwrap_or(false))
                }
                pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NAMEOID => {
                    let v = String::from_datum(self.datum, false).unwrap_or_default();
                    s.serialize_str(&v)
                }
                pg_sys::JSONOID | pg_sys::JSONBOID => {
                    let v = pgrx::JsonB::from_datum(self.datum, false)
                        .map(|j| j.0)
                        .unwrap_or(Value::Null);
                    v.serialize(s)
                }
                _ => {
                    let text = output_fn_call(self.datum, self.oid);
                    s.serialize_str(&text)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DeserializeSeed for direct V8 → Datum conversion (no JSON intermediary)
// ---------------------------------------------------------------------------

/// A [`DeserializeSeed`] that converts a deserialized V8 value directly into a
/// Postgres [`pg_sys::Datum`], using the supplied OID for type dispatch.
///
/// Returns `(Datum, isnull)`.
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
        write!(
            f,
            "a value convertible to Postgres datum (OID {:?})",
            self.oid
        )
    }

    // JS null / undefined
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok((pg_sys::Datum::from(0usize), true))
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok((pg_sys::Datum::from(0usize), true))
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::BOOLOID => v.into_datum().unwrap(),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => v.to_string().into_datum().unwrap(),
            _ => (v as i32).into_datum().unwrap(),
        };
        Ok((datum, false))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::INT2OID => (v as i16).into_datum().unwrap(),
            pg_sys::INT4OID => (v as i32).into_datum().unwrap(),
            pg_sys::INT8OID => v.into_datum().unwrap(),
            pg_sys::FLOAT4OID => (v as f32).into_datum().unwrap(),
            pg_sys::FLOAT8OID => (v as f64).into_datum().unwrap(),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => v.to_string().into_datum().unwrap(),
            pg_sys::BOOLOID => (v != 0).into_datum().unwrap(),
            _ => input_fn_call(&v.to_string(), self.oid),
        };
        Ok((datum, false))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::INT2OID => (v as i16).into_datum().unwrap(),
            pg_sys::INT4OID => (v as i32).into_datum().unwrap(),
            pg_sys::INT8OID => (v as i64).into_datum().unwrap(),
            pg_sys::FLOAT4OID => (v as f32).into_datum().unwrap(),
            pg_sys::FLOAT8OID => (v as f64).into_datum().unwrap(),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => v.to_string().into_datum().unwrap(),
            pg_sys::BOOLOID => (v != 0).into_datum().unwrap(),
            _ => input_fn_call(&v.to_string(), self.oid),
        };
        Ok((datum, false))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::INT2OID => (v as i16).into_datum().unwrap(),
            pg_sys::INT4OID => (v as i32).into_datum().unwrap(),
            pg_sys::INT8OID => (v as i64).into_datum().unwrap(),
            pg_sys::FLOAT4OID => (v as f32).into_datum().unwrap(),
            pg_sys::FLOAT8OID => v.into_datum().unwrap(),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => v.to_string().into_datum().unwrap(),
            pg_sys::BOOLOID => (v != 0.0).into_datum().unwrap(),
            _ => input_fn_call(&v.to_string(), self.oid),
        };
        Ok((datum, false))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NAMEOID => {
                v.into_datum().unwrap()
            }
            pg_sys::INT4OID => v.parse::<i32>().unwrap_or(0).into_datum().unwrap(),
            pg_sys::INT8OID => v.parse::<i64>().unwrap_or(0).into_datum().unwrap(),
            pg_sys::FLOAT8OID => v.parse::<f64>().unwrap_or(0.0).into_datum().unwrap(),
            pg_sys::BOOLOID => matches!(v.to_lowercase().as_str(), "true" | "1" | "yes" | "on")
                .into_datum()
                .unwrap(),
            pg_sys::JSONOID | pg_sys::JSONBOID => {
                let jb = pgrx::JsonB(
                    serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_owned())),
                );
                jb.into_datum().unwrap()
            }
            _ => input_fn_call(v, self.oid),
        };
        Ok((datum, false))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
        self.visit_str(&v)
    }

    // Objects — collect the recursive structure into a Value (unavoidable), then convert.
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        let value = Value::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        let datum = match self.oid {
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(value).into_datum().unwrap(),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => value.to_string().into_datum().unwrap(),
            _ => input_fn_call(&value.to_string(), self.oid),
        };
        Ok((datum, false))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        let value = Value::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
        let datum = match self.oid {
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(value).into_datum().unwrap(),
            pg_sys::TEXTOID | pg_sys::VARCHAROID => value.to_string().into_datum().unwrap(),
            _ => input_fn_call(&value.to_string(), self.oid),
        };
        Ok((datum, false))
    }
}

/// Call the type's output function to convert a datum to a string.
unsafe fn output_fn_call(datum: pg_sys::Datum, type_oid: pg_sys::Oid) -> String {
    unsafe {
        let mut output_fn: pg_sys::Oid = pg_sys::InvalidOid;
        let mut is_varlena: bool = false;
        pg_sys::getTypeOutputInfo(type_oid, &mut output_fn, &mut is_varlena);
        let cstr = pg_sys::OidOutputFunctionCall(output_fn, datum);
        let result = std::ffi::CStr::from_ptr(cstr).to_string_lossy().to_string();
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
