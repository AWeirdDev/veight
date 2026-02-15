use pyo3::prelude::*;

mod bridging;
mod isolates;
mod stack;
mod transmit;

#[pymodule]
fn veight(_py: Python<'_>, m: Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<isolates::PyIsolates>()?;
    m.add_class::<isolates::PyIsolate>()?;

    // other types
    m.add_class::<bridging::PyUndefined>()?;
    m.add_class::<bridging::PyValue>()?;

    // exceptions
    m.add("JsError", m.py().get_type::<bridging::JsError>())?;
    Ok(())
}
