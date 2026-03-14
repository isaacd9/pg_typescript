use pgrx::{pg_sys, FromDatum, IntoDatum};
use serde::de::{Deserialize, DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor};
use serde::Serialize;
use serde_json::Value;
use std::convert::TryFrom;

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
// DeserializeSeed implementations
// ---------------------------------------------------------------------------

/// A [`DeserializeSeed`] that discards the deserialized value.
pub struct VoidSeed;

impl<'de> DeserializeSeed<'de> for VoidSeed {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(())
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

    fn visit_i8<E: serde::de::Error>(self, v: i8) -> Result<Self::Value, E> {
        self.visit_i64(v as i64)
    }

    fn visit_i16<E: serde::de::Error>(self, v: i16) -> Result<Self::Value, E> {
        self.visit_i64(v as i64)
    }

    fn visit_i32<E: serde::de::Error>(self, v: i32) -> Result<Self::Value, E> {
        self.visit_i64(v as i64)
    }

    fn visit_u8<E: serde::de::Error>(self, v: u8) -> Result<Self::Value, E> {
        self.visit_u64(v as u64)
    }

    fn visit_u16<E: serde::de::Error>(self, v: u16) -> Result<Self::Value, E> {
        self.visit_u64(v as u64)
    }

    fn visit_u32<E: serde::de::Error>(self, v: u32) -> Result<Self::Value, E> {
        self.visit_u64(v as u64)
    }

    fn visit_f32<E: serde::de::Error>(self, v: f32) -> Result<Self::Value, E> {
        self.visit_f64(v as f64)
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
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(Value::Bool(v)).into_datum().unwrap(),
            oid if classify_oid(oid) != OidCategory::Scalar => input_fn_call(&v.to_string(), oid),
            _ => return Err(E::custom(type_mismatch_message(self.oid, "boolean"))),
        };
        Ok((datum, false))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::INT2OID => i16::try_from(v)
                .map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int2: {v}"))
                })?
                .into_datum()
                .unwrap(),
            pg_sys::INT4OID => i32::try_from(v)
                .map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int4: {v}"))
                })?
                .into_datum()
                .unwrap(),
            pg_sys::INT8OID => v.into_datum().unwrap(),
            pg_sys::FLOAT4OID => (v as f32).into_datum().unwrap(),
            pg_sys::FLOAT8OID => (v as f64).into_datum().unwrap(),
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(Value::from(v)).into_datum().unwrap(),
            oid if classify_oid(oid) != OidCategory::Scalar => input_fn_call(&v.to_string(), oid),
            _ => return Err(E::custom(type_mismatch_message(self.oid, "number"))),
        };
        Ok((datum, false))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::INT2OID => i16::try_from(v)
                .map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int2: {v}"))
                })?
                .into_datum()
                .unwrap(),
            pg_sys::INT4OID => i32::try_from(v)
                .map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int4: {v}"))
                })?
                .into_datum()
                .unwrap(),
            pg_sys::INT8OID => i64::try_from(v)
                .map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int8: {v}"))
                })?
                .into_datum()
                .unwrap(),
            pg_sys::FLOAT4OID => (v as f32).into_datum().unwrap(),
            pg_sys::FLOAT8OID => (v as f64).into_datum().unwrap(),
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(Value::from(v)).into_datum().unwrap(),
            oid if classify_oid(oid) != OidCategory::Scalar => input_fn_call(&v.to_string(), oid),
            _ => return Err(E::custom(type_mismatch_message(self.oid, "number"))),
        };
        Ok((datum, false))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::INT2OID => {
                if !v.is_finite() || v.fract() != 0.0 {
                    return Err(E::custom(format!(
                        "pg_typescript: expected integral number for int2, got {v}"
                    )));
                }
                let iv = i64::try_from(v as i128).map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int2: {v}"))
                })?;
                i16::try_from(iv)
                    .map_err(|_| {
                        E::custom(format!("pg_typescript: integer out of range for int2: {v}"))
                    })?
                    .into_datum()
                    .unwrap()
            }
            pg_sys::INT4OID => {
                if !v.is_finite() || v.fract() != 0.0 {
                    return Err(E::custom(format!(
                        "pg_typescript: expected integral number for int4, got {v}"
                    )));
                }
                let iv = i64::try_from(v as i128).map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int4: {v}"))
                })?;
                i32::try_from(iv)
                    .map_err(|_| {
                        E::custom(format!("pg_typescript: integer out of range for int4: {v}"))
                    })?
                    .into_datum()
                    .unwrap()
            }
            pg_sys::INT8OID => {
                if !v.is_finite() || v.fract() != 0.0 {
                    return Err(E::custom(format!(
                        "pg_typescript: expected integral number for int8, got {v}"
                    )));
                }
                let iv = i64::try_from(v as i128).map_err(|_| {
                    E::custom(format!("pg_typescript: integer out of range for int8: {v}"))
                })?;
                iv.into_datum().unwrap()
            }
            pg_sys::FLOAT4OID => (v as f32).into_datum().unwrap(),
            pg_sys::FLOAT8OID => v.into_datum().unwrap(),
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(Value::from(v)).into_datum().unwrap(),
            oid if classify_oid(oid) != OidCategory::Scalar => input_fn_call(&v.to_string(), oid),
            _ => return Err(E::custom(type_mismatch_message(self.oid, "number"))),
        };
        Ok((datum, false))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let datum = match self.oid {
            pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NAMEOID => {
                v.into_datum().unwrap()
            }
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(Value::String(v.to_owned()))
                .into_datum()
                .unwrap(),
            oid if classify_oid(oid) != OidCategory::Scalar => input_fn_call(v, oid),
            _ => return Err(E::custom(type_mismatch_message(self.oid, "string"))),
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
            oid if classify_oid(oid) == OidCategory::Composite => {
                return match value {
                    Value::Object(obj) => unsafe { build_heap_tuple(obj, oid) }
                        .map(|d| (d, false))
                        .map_err(A::Error::custom),
                    _ => Err(A::Error::custom(
                        "pg_typescript: composite return type requires a JS object",
                    )),
                };
            }
            oid if classify_oid(oid) == OidCategory::Other => {
                input_fn_call(&value.to_string(), oid)
            }
            _ => return Err(A::Error::custom(type_mismatch_message(self.oid, "object"))),
        };
        Ok((datum, false))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        let value = Value::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
        let datum = match self.oid {
            pg_sys::JSONOID | pg_sys::JSONBOID => pgrx::JsonB(value).into_datum().unwrap(),
            oid if classify_oid(oid) != OidCategory::Scalar => {
                input_fn_call(&value.to_string(), oid)
            }
            _ => return Err(A::Error::custom(type_mismatch_message(self.oid, "array"))),
        };
        Ok((datum, false))
    }
}

fn expected_sql_type(oid: pg_sys::Oid) -> &'static str {
    match oid {
        pg_sys::BOOLOID => "boolean",
        pg_sys::INT2OID => "int2",
        pg_sys::INT4OID => "int4",
        pg_sys::INT8OID => "int8",
        pg_sys::FLOAT4OID => "float4",
        pg_sys::FLOAT8OID => "float8",
        pg_sys::TEXTOID => "text",
        pg_sys::VARCHAROID => "varchar",
        pg_sys::BPCHAROID => "char",
        pg_sys::NAMEOID => "name",
        pg_sys::JSONOID => "json",
        pg_sys::JSONBOID => "jsonb",
        _ => "declared SQL return type",
    }
}

fn type_mismatch_message(oid: pg_sys::Oid, got: &str) -> String {
    format!(
        "pg_typescript: return type mismatch: expected {}, got {got}",
        expected_sql_type(oid)
    )
}

/// Categorise a Postgres OID so callers can decide how to convert a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OidCategory {
    /// A well-known scalar type handled with dedicated conversion logic.
    Scalar,
    /// A composite (row) type — convert JS object fields to tuple columns.
    Composite,
    /// Anything else — fall back to the type's input/output functions.
    Other,
}

fn classify_oid(oid: pg_sys::Oid) -> OidCategory {
    match oid {
        pg_sys::BOOLOID
        | pg_sys::INT2OID
        | pg_sys::INT4OID
        | pg_sys::INT8OID
        | pg_sys::FLOAT4OID
        | pg_sys::FLOAT8OID
        | pg_sys::TEXTOID
        | pg_sys::VARCHAROID
        | pg_sys::BPCHAROID
        | pg_sys::NAMEOID
        | pg_sys::JSONOID
        | pg_sys::JSONBOID => OidCategory::Scalar,
        oid if unsafe { (pg_sys::get_typtype(oid) as u8) == b'c' } => OidCategory::Composite,
        _ => OidCategory::Other,
    }
}

/// Build a composite (row) datum from a JSON object typed by `type_oid`.
///
/// Fields absent from the object become SQL NULL; extra fields are ignored.
unsafe fn build_heap_tuple(
    obj: serde_json::Map<String, Value>,
    type_oid: pg_sys::Oid,
) -> Result<pg_sys::Datum, String> {
    // PgTupleDesc handles PG-version differences in attribute layout and
    // decrements the refcount automatically on drop.
    let tupdesc = pgrx::PgTupleDesc::for_composite_type_by_oid(type_oid)
        .ok_or_else(|| format!("pg_typescript: OID {type_oid:?} is not a composite type"))?;
    let natts = tupdesc.len();

    let mut datums = vec![pg_sys::Datum::from(0usize); natts];
    let mut nulls = vec![true; natts]; // default all columns to NULL

    for i in 0..natts {
        let attr = tupdesc.get(i).unwrap();
        if attr.attisdropped {
            continue;
        }

        if let Some(field_val) = obj.get(attr.name()) {
            let (datum, isnull) = PgDatumSeed { oid: attr.atttypid }
                .deserialize(field_val.clone())
                .map_err(|e: serde_json::Error| e.to_string())?;
            datums[i] = datum;
            nulls[i] = isnull;
        }
    }

    let tuple = pg_sys::heap_form_tuple(tupdesc.as_ptr(), datums.as_mut_ptr(), nulls.as_mut_ptr());

    // HeapTupleGetDatum: PointerGetDatum(tuple->t_data)
    Ok(pg_sys::Datum::from((*tuple).t_data as usize))
}

// ---------------------------------------------------------------------------
// DeserializeSeed for RETURNS RECORD (anonymous composite via TupleDesc)
// ---------------------------------------------------------------------------

/// Unified seed for return-value deserialization.  Wraps either a simple OID
/// (named type) or a pre-resolved [`TupleDesc`] (RETURNS RECORD).
pub enum ReturnSeed {
    Oid(PgDatumSeed),
    Record(pg_sys::TupleDesc),
}

impl<'de> DeserializeSeed<'de> for ReturnSeed {
    type Value = (pg_sys::Datum, bool);

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        match self {
            Self::Oid(s) => s.deserialize(deserializer),
            Self::Record(tupdesc) => {
                deserializer.deserialize_any(RecordDatumVisitor { tupdesc })
            }
        }
    }
}

struct RecordDatumVisitor {
    tupdesc: pg_sys::TupleDesc,
}

impl<'de> Visitor<'de> for RecordDatumVisitor {
    type Value = (pg_sys::Datum, bool);

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a JS object convertible to a Postgres RECORD")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok((pg_sys::Datum::from(0usize), true))
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok((pg_sys::Datum::from(0usize), true))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        let value = Value::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        match value {
            Value::Object(obj) => unsafe { build_heap_tuple_from_tupdesc(obj, self.tupdesc) }
                .map(|d| (d, false))
                .map_err(A::Error::custom),
            _ => Err(A::Error::custom(
                "pg_typescript: RECORD return type requires a JS object",
            )),
        }
    }
}

/// Build a composite (row) datum from a JSON object using a pre-resolved
/// [`TupleDesc`].  The descriptor must already be blessed (via
/// `BlessTupleDesc`).
unsafe fn build_heap_tuple_from_tupdesc(
    obj: serde_json::Map<String, Value>,
    tupdesc: pg_sys::TupleDesc,
) -> Result<pg_sys::Datum, String> {
    let natts = (*tupdesc).natts as usize;

    let mut datums = vec![pg_sys::Datum::from(0usize); natts];
    let mut nulls = vec![true; natts];

    for i in 0..natts {
        let attr = &*pg_sys::TupleDescAttr(tupdesc, i as i32);
        if attr.attisdropped {
            continue;
        }

        if let Some(field_val) = obj.get(attr.name()) {
            let (datum, isnull) = PgDatumSeed { oid: attr.atttypid }
                .deserialize(field_val.clone())
                .map_err(|e: serde_json::Error| e.to_string())?;
            datums[i] = datum;
            nulls[i] = isnull;
        }
    }

    let tuple = pg_sys::heap_form_tuple(tupdesc, datums.as_mut_ptr(), nulls.as_mut_ptr());
    Ok(pg_sys::Datum::from((*tuple).t_data as usize))
}

/// Call the type's output function to convert a datum to a string.
unsafe fn output_fn_call(datum: pg_sys::Datum, type_oid: pg_sys::Oid) -> String {
    let mut output_fn: pg_sys::Oid = pg_sys::InvalidOid;
    let mut is_varlena: bool = false;
    pg_sys::getTypeOutputInfo(type_oid, &mut output_fn, &mut is_varlena);
    let cstr = pg_sys::OidOutputFunctionCall(output_fn, datum);
    let result = std::ffi::CStr::from_ptr(cstr)
        .to_string_lossy()
        .into_owned();
    pg_sys::pfree(cstr.cast());
    result
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
