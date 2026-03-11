use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

use deno_core::{op2, JsRuntime, OpState};
use deno_error::JsErrorBox;
use pgrx::{pg_sys, FromDatum, IntoDatum, JsonB, PgTryBuilder, Spi};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const PG_HOOK_JS: &str = include_str!("../js/pg_hook.js");

#[derive(Clone, Copy, Default)]
pub struct PgExecuteAllowed(pub bool);

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireParam {
    Inferred {
        value: WireValue,
    },
    Typed {
        #[serde(rename = "type")]
        type_ref: WireTypeRef,
        value: WireValue,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireTypeRef {
    Name { value: String },
    Oid { value: u32 },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireValue {
    Null,
    Bool { value: bool },
    Number { value: f64 },
    String { value: String },
    Bigint { value: String },
    Array { value: Vec<WireValue> },
    Object { value: BTreeMap<String, WireValue> },
}

#[derive(Debug)]
struct SpiParam {
    oid: pg_sys::Oid,
    datum: pg_sys::Datum,
    is_null: bool,
}

#[derive(Debug)]
pub struct PgExecuteResult {
    rows: Vec<PgExecuteRow>,
    command: &'static str,
    row_count: f64,
}

#[derive(Debug)]
struct PgExecuteRow(Vec<(String, OwnedPgValue)>);

#[derive(Debug)]
enum OwnedPgValue {
    Null,
    Bool(bool),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    String(String),
    Json(JsonValue),
}

#[op2]
#[serde]
pub fn op_pg_execute(
    state: &mut OpState,
    #[string] sql: &str,
    #[serde] params: Vec<WireParam>,
) -> Result<PgExecuteResult, JsErrorBox> {
    if !state.borrow::<PgExecuteAllowed>().0 {
        return Err(JsErrorBox::generic(
            "pg_typescript: _pg.execute() may only be called from inside the function body",
        ));
    }

    PgTryBuilder::new(|| execute_sql(sql, params))
        .catch_others(|cause| {
            let message = match cause {
                pgrx::pg_sys::panic::CaughtError::PostgresError(err)
                | pgrx::pg_sys::panic::CaughtError::ErrorReport(err)
                | pgrx::pg_sys::panic::CaughtError::RustPanic { ereport: err, .. } => {
                    format_pg_error(&err)
                }
            };
            Err(JsErrorBox::generic(message))
        })
        .execute()
}

fn execute_sql(sql: &str, params: Vec<WireParam>) -> Result<PgExecuteResult, JsErrorBox> {
    let spi_params = params
        .into_iter()
        .enumerate()
        .map(|(idx, param)| bind_param(param).map_err(|err| param_error(idx, err)))
        .collect::<Result<Vec<_>, _>>()?;

    Spi::connect(|_client| {
        let query =
            CString::new(sql).map_err(|e| JsErrorBox::generic(format!("pg_typescript: {e}")))?;

        let status_code = execute_spi(&query, &spi_params);
        let command = map_command(status_code)?;
        let row_count = unsafe { pg_sys::SPI_processed as u64 } as f64;
        let rows = extract_rows()?;

        Ok(PgExecuteResult {
            rows,
            command,
            row_count,
        })
    })
}

fn bind_param(param: WireParam) -> Result<SpiParam, JsErrorBox> {
    match param {
        WireParam::Inferred { value } => bind_inferred_value(value),
        WireParam::Typed { type_ref, value } => bind_typed_value(type_ref, value),
    }
}

fn bind_inferred_value(value: WireValue) -> Result<SpiParam, JsErrorBox> {
    match value {
        WireValue::Null => Ok(SpiParam {
            oid: pg_sys::UNKNOWNOID,
            datum: pg_sys::Datum::from(0usize),
            is_null: true,
        }),
        WireValue::Bool { value } => Ok(SpiParam {
            oid: pg_sys::BOOLOID,
            datum: value
                .into_datum()
                .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode boolean"))?,
            is_null: false,
        }),
        WireValue::Number { value } => bind_inferred_number(value),
        WireValue::String { value } => Ok(SpiParam {
            oid: pg_sys::TEXTOID,
            datum: value
                .into_datum()
                .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode text"))?,
            is_null: false,
        }),
        WireValue::Bigint { value } => {
            let parsed = value.parse::<i64>().map_err(|_| {
                JsErrorBox::range_error(format!(
                    "pg_typescript: bigint parameter is out of range for int8: {value}"
                ))
            })?;
            Ok(SpiParam {
                oid: pg_sys::INT8OID,
                datum: parsed
                    .into_datum()
                    .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode int8"))?,
                is_null: false,
            })
        }
        complex @ (WireValue::Array { .. } | WireValue::Object { .. }) => {
            let json = complex.to_json_value()?;
            Ok(SpiParam {
                oid: pg_sys::JSONBOID,
                datum: JsonB(json)
                    .into_datum()
                    .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode jsonb"))?,
                is_null: false,
            })
        }
    }
}

fn bind_inferred_number(value: f64) -> Result<SpiParam, JsErrorBox> {
    if value.fract() == 0.0 && value >= i32::MIN as f64 && value <= i32::MAX as f64 {
        let parsed = value as i32;
        return Ok(SpiParam {
            oid: pg_sys::INT4OID,
            datum: parsed
                .into_datum()
                .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode int4"))?,
            is_null: false,
        });
    }

    if value.fract() == 0.0 {
        let parsed = value as i64;
        return Ok(SpiParam {
            oid: pg_sys::INT8OID,
            datum: parsed
                .into_datum()
                .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode int8"))?,
            is_null: false,
        });
    }

    Ok(SpiParam {
        oid: pg_sys::FLOAT8OID,
        datum: value
            .into_datum()
            .ok_or_else(|| JsErrorBox::generic("pg_typescript: failed to encode float8"))?,
        is_null: false,
    })
}

fn bind_typed_value(type_ref: WireTypeRef, value: WireValue) -> Result<SpiParam, JsErrorBox> {
    let oid = resolve_type_ref(type_ref);

    match value {
        WireValue::Null => Ok(SpiParam {
            oid,
            datum: pg_sys::Datum::from(0usize),
            is_null: true,
        }),
        complex @ (WireValue::Array { .. } | WireValue::Object { .. })
            if oid == pg_sys::JSONOID || oid == pg_sys::JSONBOID =>
        {
            let text = serde_json::to_string(&complex.to_json_value()?)
                .map_err(|e| JsErrorBox::generic(format!("pg_typescript: {e}")))?;
            Ok(SpiParam {
                oid,
                datum: input_fn_call(&text, oid),
                is_null: false,
            })
        }
        WireValue::Array { .. } | WireValue::Object { .. } => Err(JsErrorBox::type_error(
            "pg_typescript: array/object typed parameters are only supported for json/jsonb",
        )),
        primitive => {
            let text = primitive.to_text()?;
            Ok(SpiParam {
                oid,
                datum: input_fn_call(&text, oid),
                is_null: false,
            })
        }
    }
}

fn resolve_type_ref(type_ref: WireTypeRef) -> pg_sys::Oid {
    match type_ref {
        WireTypeRef::Oid { value } => pg_sys::Oid::from(value),
        WireTypeRef::Name { value } => pgrx::regtypein(&value),
    }
}

fn execute_spi(query: &CString, params: &[SpiParam]) -> i32 {
    unsafe {
        pg_sys::SPI_tuptable = std::ptr::null_mut();
    }

    match params.len() {
        0 => unsafe { pg_sys::SPI_execute(query.as_ptr(), false, 0) },
        nargs => {
            let mut argtypes = params.iter().map(|param| param.oid).collect::<Vec<_>>();
            let mut datums = params.iter().map(|param| param.datum).collect::<Vec<_>>();
            let nulls = params
                .iter()
                .map(|param| {
                    if param.is_null {
                        b'n' as c_char
                    } else {
                        b' ' as c_char
                    }
                })
                .collect::<Vec<_>>();

            unsafe {
                pg_sys::SPI_execute_with_args(
                    query.as_ptr(),
                    nargs as i32,
                    argtypes.as_mut_ptr(),
                    datums.as_mut_ptr(),
                    nulls.as_ptr(),
                    false,
                    0,
                )
            }
        }
    }
}

fn extract_rows() -> Result<Vec<PgExecuteRow>, JsErrorBox> {
    let Some(table) = (unsafe { pg_sys::SPI_tuptable.as_ref() }) else {
        return Ok(Vec::new());
    };

    let tupdesc = table.tupdesc;
    let num_rows = table.numvals as usize;
    let num_cols = unsafe { (*tupdesc).natts as usize };
    let tuples = unsafe { std::slice::from_raw_parts(table.vals, num_rows) };

    let mut rows = Vec::with_capacity(num_rows);

    for tuple in tuples {
        let mut row = Vec::with_capacity(num_cols);
        for ordinal in 1..=num_cols {
            let name = column_name(tupdesc, ordinal as i32)?;
            let mut is_null = false;
            let datum =
                unsafe { pg_sys::SPI_getbinval(*tuple, tupdesc, ordinal as i32, &mut is_null) };
            let oid = unsafe { pg_sys::SPI_gettypeid(tupdesc, ordinal as i32) };
            let value = OwnedPgValue::from_datum(datum, is_null, oid)?;
            row.push((name, value));
        }
        rows.push(PgExecuteRow(row));
    }

    Ok(rows)
}

fn column_name(tupdesc: pg_sys::TupleDesc, ordinal: i32) -> Result<String, JsErrorBox> {
    unsafe {
        let ptr = pg_sys::SPI_fname(tupdesc, ordinal);
        if ptr.is_null() {
            return Err(JsErrorBox::generic(format!(
                "pg_typescript: could not read SPI column name at ordinal {ordinal}"
            )));
        }

        let name = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        pg_sys::pfree(ptr.cast());
        Ok(name)
    }
}

fn map_command(status_code: i32) -> Result<&'static str, JsErrorBox> {
    if status_code == 0 {
        return Ok("EMPTY");
    }

    match Spi::check_status(status_code)
        .map_err(|e| JsErrorBox::generic(format!("pg_typescript: SPI execute failed: {e}")))?
    {
        pgrx::spi::SpiOkCodes::Select | pgrx::spi::SpiOkCodes::SelInto => Ok("SELECT"),
        pgrx::spi::SpiOkCodes::Insert | pgrx::spi::SpiOkCodes::InsertReturning => Ok("INSERT"),
        pgrx::spi::SpiOkCodes::Update | pgrx::spi::SpiOkCodes::UpdateReturning => Ok("UPDATE"),
        pgrx::spi::SpiOkCodes::Delete | pgrx::spi::SpiOkCodes::DeleteReturning => Ok("DELETE"),
        pgrx::spi::SpiOkCodes::Merge => Ok("MERGE"),
        _ => Ok("UTILITY"),
    }
}

fn param_error(index: usize, err: JsErrorBox) -> JsErrorBox {
    JsErrorBox::generic(format!(
        "pg_typescript: invalid parameter ${}: {}",
        index + 1,
        err
    ))
}

fn format_pg_error(err: &pgrx::pg_sys::panic::ErrorReportWithLevel) -> String {
    let mut message = format!("pg_typescript: {}", err.message());
    if let Some(detail) = err.detail() {
        message.push_str(&format!("\nDETAIL: {detail}"));
    }
    if let Some(hint) = err.hint() {
        message.push_str(&format!("\nHINT: {hint}"));
    }
    message
}

impl WireValue {
    fn to_text(&self) -> Result<String, JsErrorBox> {
        match self {
            Self::Null => Err(JsErrorBox::type_error(
                "pg_typescript: null values require an explicit type or SQL cast",
            )),
            Self::Bool { value } => Ok(value.to_string()),
            Self::Number { value } => Ok(value.to_string()),
            Self::String { value } => Ok(value.clone()),
            Self::Bigint { value } => Ok(value.clone()),
            Self::Array { .. } | Self::Object { .. } => Err(JsErrorBox::type_error(
                "pg_typescript: array/object parameters require JSON/jsonb or a string literal",
            )),
        }
    }

    fn to_json_value(&self) -> Result<JsonValue, JsErrorBox> {
        match self {
            Self::Null => Ok(JsonValue::Null),
            Self::Bool { value } => Ok(JsonValue::Bool(*value)),
            Self::Number { value } => serde_json::Number::from_f64(*value)
                .map(JsonValue::Number)
                .ok_or_else(|| {
                    JsErrorBox::type_error(
                        "pg_typescript: cannot encode NaN or Infinity inside JSON values",
                    )
                }),
            Self::String { value } => Ok(JsonValue::String(value.clone())),
            Self::Bigint { .. } => Err(JsErrorBox::type_error(
                "pg_typescript: bigint values are not supported inside inferred JSON parameters",
            )),
            Self::Array { value } => value
                .iter()
                .map(WireValue::to_json_value)
                .collect::<Result<Vec<_>, _>>()
                .map(JsonValue::Array),
            Self::Object { value } => value
                .iter()
                .map(|(key, value)| Ok((key.clone(), value.to_json_value()?)))
                .collect::<Result<serde_json::Map<String, JsonValue>, _>>()
                .map(JsonValue::Object),
        }
    }
}

impl OwnedPgValue {
    fn from_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        oid: pg_sys::Oid,
    ) -> Result<Self, JsErrorBox> {
        if is_null {
            return Ok(Self::Null);
        }

        unsafe {
            match oid {
                pg_sys::INT2OID => {
                    Ok(Self::I16(i16::from_datum(datum, false).ok_or_else(
                        || JsErrorBox::generic("pg_typescript: failed to decode int2"),
                    )?))
                }
                pg_sys::INT4OID => {
                    Ok(Self::I32(i32::from_datum(datum, false).ok_or_else(
                        || JsErrorBox::generic("pg_typescript: failed to decode int4"),
                    )?))
                }
                pg_sys::INT8OID => {
                    Ok(Self::I64(i64::from_datum(datum, false).ok_or_else(
                        || JsErrorBox::generic("pg_typescript: failed to decode int8"),
                    )?))
                }
                pg_sys::FLOAT4OID => Ok(Self::F32(f32::from_datum(datum, false).ok_or_else(
                    || JsErrorBox::generic("pg_typescript: failed to decode float4"),
                )?)),
                pg_sys::FLOAT8OID => Ok(Self::F64(f64::from_datum(datum, false).ok_or_else(
                    || JsErrorBox::generic("pg_typescript: failed to decode float8"),
                )?)),
                pg_sys::BOOLOID => Ok(Self::Bool(bool::from_datum(datum, false).ok_or_else(
                    || JsErrorBox::generic("pg_typescript: failed to decode boolean"),
                )?)),
                pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID | pg_sys::NAMEOID => {
                    Ok(Self::String(String::from_datum(datum, false).ok_or_else(
                        || JsErrorBox::generic("pg_typescript: failed to decode text"),
                    )?))
                }
                pg_sys::JSONOID | pg_sys::JSONBOID => Ok(Self::Json(
                    JsonB::from_datum(datum, false)
                        .map(|json| json.0)
                        .ok_or_else(|| {
                            JsErrorBox::generic("pg_typescript: failed to decode json/jsonb")
                        })?,
                )),
                _ => Ok(Self::String(output_fn_call(datum, oid))),
            }
        }
    }
}

impl Serialize for PgExecuteResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("rows", &self.rows)?;
        map.serialize_entry("command", self.command)?;
        map.serialize_entry("rowCount", &self.row_count)?;
        map.end()
    }
}

impl Serialize for PgExecuteRow {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl Serialize for OwnedPgValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::I16(value) => serializer.serialize_i16(*value),
            Self::I32(value) => serializer.serialize_i32(*value),
            Self::I64(value) => serializer.serialize_i64(*value),
            Self::F32(value) => serializer.serialize_f32(*value),
            Self::F64(value) => serializer.serialize_f64(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::Json(value) => value.serialize(serializer),
        }
    }
}

fn output_fn_call(datum: pg_sys::Datum, type_oid: pg_sys::Oid) -> String {
    unsafe {
        let mut output_fn: pg_sys::Oid = pg_sys::InvalidOid;
        let mut is_varlena = false;
        pg_sys::getTypeOutputInfo(type_oid, &mut output_fn, &mut is_varlena);
        let cstr = pg_sys::OidOutputFunctionCall(output_fn, datum);
        let result = CStr::from_ptr(cstr).to_string_lossy().into_owned();
        pg_sys::pfree(cstr.cast());
        result
    }
}

fn input_fn_call(value: &str, type_oid: pg_sys::Oid) -> pg_sys::Datum {
    unsafe {
        let mut input_fn: pg_sys::Oid = pg_sys::InvalidOid;
        let mut ioparam: pg_sys::Oid = pg_sys::InvalidOid;
        pg_sys::getTypeInputInfo(type_oid, &mut input_fn, &mut ioparam);
        let cstr = CString::new(value).expect("NUL byte in parameter text");
        pg_sys::OidInputFunctionCall(input_fn, cstr.as_ptr().cast_mut(), ioparam, -1)
    }
}

deno_core::extension!(
    pg_typescript_pg,
    ops = [op_pg_execute],
    esm_entry_point = "ext:pg_typescript_pg/pg_bridge.js",
    esm = [ dir "src/js", "pg_bridge.js" ],
    state = |state| {
        if !state.has::<PgExecuteAllowed>() {
            state.put(PgExecuteAllowed::default());
        }
    },
);

pub fn install_pg_api(rt: &mut JsRuntime) {
    rt.execute_script("pg_typescript:pg_hook", PG_HOOK_JS)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to install _pg API: {e}"));
}

pub fn ensure_pg_api(rt: &mut JsRuntime) {
    install_pg_api(rt);
}

pub fn set_pg_execute_allowed(rt: &mut JsRuntime, allowed: bool) {
    rt.op_state()
        .borrow_mut()
        .put::<PgExecuteAllowed>(PgExecuteAllowed(allowed));
}

pub fn with_pg_execute_allowed<F, R>(rt: &mut JsRuntime, f: F) -> R
where
    F: FnOnce(&mut JsRuntime) -> R,
{
    set_pg_execute_allowed(rt, true);
    let result = catch_unwind(AssertUnwindSafe(|| f(rt)));
    set_pg_execute_allowed(rt, false);
    match result {
        Ok(result) => result,
        Err(caught) => resume_unwind(caught),
    }
}

#[cfg(test)]
mod tests {
    use super::{map_command, WireValue};

    #[test]
    fn wire_value_json_rejects_bigint() {
        let err = WireValue::Bigint {
            value: "42".to_string(),
        }
        .to_json_value()
        .unwrap_err();
        assert!(err.to_string().contains("bigint values are not supported"));
    }

    #[test]
    fn empty_command_maps() {
        assert_eq!(map_command(0).unwrap(), "EMPTY");
    }
}
