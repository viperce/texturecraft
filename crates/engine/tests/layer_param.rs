//! A present but invalid `layer` param is an error, not a fallback to the selected layer.

use serde_json::json;
use texturecraft_engine::geom::primitives;
use texturecraft_engine::{Session, execute};

#[test]
fn invalid_layer_id_is_rejected() {
    let mut s = Session::new_project("t", primitives::plane(1.0, 1), "builtin:plane", None, 128).unwrap();
    execute(&mut s, "layer.add.fill", &json!({})).unwrap();
    let before = s.selected_layer();
    for bad in [json!(-1), json!("x")] {
        let r = execute(&mut s, "layer.remove", &json!({ "layer": bad }));
        assert!(r.is_err(), "layer {bad} must be rejected, got {r:?}");
        assert_eq!(s.selected_layer(), before);
    }
    assert!(execute(&mut s, "layer.remove", &json!({})).is_ok());
}
