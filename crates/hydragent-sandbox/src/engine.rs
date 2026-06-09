use wasmtime::{Config, Engine};

pub fn create_sandbox_engine() -> anyhow::Result<Engine> {
    let mut config = Config::new();
    config.consume_fuel(true); // Enable CPU instruction fuel metering
    config.wasm_component_model(false); // Enable classic WASM module mode
    let engine = Engine::new(&config)?;
    Ok(engine)
}
