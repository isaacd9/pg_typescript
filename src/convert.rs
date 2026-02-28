use pgrx::{FromDatum, IntoDatum, pg_sys};
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
