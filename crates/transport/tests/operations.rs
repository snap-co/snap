use snap_transport::{
    Error,
    operation::{Definition, Registry},
};

fn definition(name: &str) -> Definition {
    Definition {
        name: name.into(),
        identity_required: false,
        input: |_| true,
        output: |_| true,
        progress: |_| false,
        error: |_| true,
        guards: vec![],
        inputs: &[],
        data: snap_store::Data::new(&[]),
        handler: snap_transport::operation::Handler::new(|_, call, _, _, _| Ok(call.input.clone())),
    }
}

#[test]
fn assembly_rejects_ambiguous_names_without_replacing_a_selected_definition() {
    let mut operations = Registry::default();
    let first = operations.register(definition("fixture.first")).unwrap();
    for invalid in [
        "",
        "bare",
        ".first",
        "fixture.",
        "fixture..first",
        "fixture first",
        "fixture.first",
    ] {
        assert_eq!(
            operations.register(definition(invalid)),
            Err(Error::Protocol)
        );
    }
    let second = operations
        .register_preconnection(definition("fixture.second"))
        .unwrap();
    assert_eq!(
        operations.register_preconnection(definition("fixture.first")),
        Err(Error::Protocol)
    );
    assert!(!operations.is_preconnection(first));
    assert!(operations.is_preconnection(second));
    assert_eq!(operations.resolve("fixture.first"), Ok(first));
    assert_eq!(operations.get(first).name, "fixture.first");
    assert_eq!(operations.resolve("fixture.second"), Ok(second));
    assert_eq!(operations.get(second).name, "fixture.second");
    assert_eq!(
        operations.resolve("fixture.missing"),
        Err(Error::UnknownOperation)
    );
}
