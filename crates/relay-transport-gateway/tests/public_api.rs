use relay_transport_gateway::{RemoteRunFilterV5, RunQueryActionV5};

#[test]
fn run_query_action_validation_is_part_of_the_public_api() {
    let action = RunQueryActionV5::ListRuns {
        filter: RemoteRunFilterV5::Recent,
        page_size: 10,
        cursor: None,
    };

    assert_eq!(action.validate(), Ok(()));
}
