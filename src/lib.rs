use pyo3::prelude::*;

mod bridging;
mod isolates;
mod stack;
mod transmit;

#[pymodule]
fn veight(_py: Python<'_>, m: Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<isolates::PyIsolates>()?;

    // other types
    m.add_class::<bridging::PyUndefined>()?;
    m.add_class::<bridging::PyValue>()?;
    Ok(())
}
