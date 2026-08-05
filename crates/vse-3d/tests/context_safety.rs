use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use vision_stimulus_engine::prelude::*;
use vse_3d::{ModelError, ModelHandle, Vse3d, Vse3dConfig, Vse3dError};

fn bunny() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("assets/3d/models/bunny.glb")
}

#[test]
fn a_model_handle_cannot_resolve_to_the_same_numeric_id_in_another_renderer() {
    let foreign = Cell::new(None::<ModelHandle>);
    let mut first = HeadlessContext::builder(32, 32).build().unwrap();
    first
        .run_headless_with_setup(
            |vse| {
                let mut renderer = Vse3d::register(vse, Vse3dConfig::default())?;
                let model = renderer.load_model(bunny())?;
                Ok((renderer, model))
            },
            |_frame| Ok(()),
            |vse, (_renderer, model)| {
                foreign.set(Some(*model));
                vse.request_exit();
                Ok(())
            },
        )
        .unwrap();

    let observed = RefCell::new(None::<VSEError>);
    let mut second = HeadlessContext::builder(32, 32).build().unwrap();
    second
        .run_headless_with_setup(
            |vse| {
                let mut renderer = Vse3d::register(vse, Vse3dConfig::default())?;
                let own_model = renderer.load_model(bunny())?;
                Ok((renderer, own_model))
            },
            |_frame| Ok(()),
            |vse, (renderer, own_model)| {
                assert!(renderer.model_info(*own_model).is_ok());
                *observed.borrow_mut() = renderer.model_info(foreign.get().unwrap()).err();
                vse.request_exit();
                Ok(())
            },
        )
        .unwrap();

    let error = observed.into_inner().expect("foreign handle must fail");
    let VSEError::Extension(source) = error else {
        panic!("expected extension error, got {error}");
    };
    assert!(matches!(
        source.downcast_ref::<Vse3dError>(),
        Some(Vse3dError::Model(ModelError::ForeignHandle))
    ));
}
