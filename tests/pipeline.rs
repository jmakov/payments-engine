//! End-to-end checks of the reusable two-worker pipeline.

#[test]
fn pending_disputes_can_settle_after_an_account_is_locked() {
    let input = b"type,client,tx,amount\n\
deposit,1,1,10\n\
deposit,1,2,5\n\
deposit,1,3,2\n\
dispute,1,1\n\
dispute,1,2\n\
chargeback,1,1\n\
resolve,1,2\n\
dispute,1,3\n\
chargeback,1,3\n";
    let mut report = Vec::new();

    payments_engine::run(input.as_slice(), &mut report).expect("valid transactions succeed");

    assert_eq!(
        report,
        b"client,available,held,total,locked\n1,5,0,5,true\n"
    );
}
