//! An in-memory ledger for tests. It records each call and returns the next
//! scripted result, or a new id when the script is empty.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::{BillBody, Ledger, LedgerError, PaymentBody, Posted};

#[derive(Clone, Default)]
struct Inner {
    script: VecDeque<Result<(), LedgerError>>,
    calls: Vec<String>,
    ids: HashMap<String, String>,
    next_bill: u64,
    next_payment: u64,
    next_file: u64,
}

/// Shared by a test and the server. `script_err` queues the next refusal.
#[derive(Clone, Default)]
pub struct FakeLedger {
    inner: Arc<Mutex<Inner>>,
}

impl FakeLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next call fails with this error. Later calls are unchanged.
    pub fn script_err(&self, error: LedgerError) {
        self.inner
            .lock()
            .expect("fake ledger")
            .script
            .push_back(Err(error));
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().expect("fake ledger").calls.clone()
    }
}

impl FakeLedger {
    fn take(&self, call: String) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().expect("fake ledger");
        inner.calls.push(call);
        match inner.script.pop_front() {
            Some(result) => result,
            None => Ok(()),
        }
    }

    fn remember(&self, key: &str, next: &mut dyn FnMut(&mut Inner) -> String) -> String {
        let mut inner = self.inner.lock().expect("fake ledger");
        if let Some(id) = inner.ids.get(key) {
            return id.clone();
        }
        let id = next(&mut inner);
        inner.ids.insert(key.to_owned(), id.clone());
        id
    }

    async fn do_post_bill(&self, body: &BillBody) -> Result<Posted, LedgerError> {
        self.take(format!("post_bill {}", body.external_key))?;
        let key = body.external_key.clone();
        let external_id = self.remember(&key, &mut |inner| {
            inner.next_bill += 1;
            format!("fake-bill-{}", inner.next_bill)
        });
        Ok(Posted {
            external_id,
            external_key: key,
        })
    }

    async fn do_post_payment(&self, body: &PaymentBody) -> Result<Posted, LedgerError> {
        self.take(format!("post_payment {}", body.external_key))?;
        let key = body.external_key.clone();
        let external_id = self.remember(&key, &mut |inner| {
            inner.next_payment += 1;
            format!("fake-payment-{}", inner.next_payment)
        });
        Ok(Posted {
            external_id,
            external_key: key,
        })
    }

    async fn do_void_bill(&self, external_id: &str) -> Result<(), LedgerError> {
        self.take(format!("void_bill {external_id}"))
    }

    async fn do_void_payment(&self, external_id: &str) -> Result<(), LedgerError> {
        self.take(format!("void_payment {external_id}"))
    }

    async fn do_attach_file(
        &self,
        bill_external_id: &str,
        name: &str,
    ) -> Result<String, LedgerError> {
        self.take(format!("attach_file {bill_external_id} {name}"))?;
        let mut inner = self.inner.lock().expect("fake ledger");
        inner.next_file += 1;
        Ok(format!("fake-file-{}", inner.next_file))
    }
}

impl Ledger for FakeLedger {
    fn post_bill<'a>(
        &'a self,
        body: &'a BillBody,
    ) -> Pin<Box<dyn Future<Output = Result<Posted, LedgerError>> + Send + 'a>> {
        Box::pin(self.do_post_bill(body))
    }

    fn post_payment<'a>(
        &'a self,
        body: &'a PaymentBody,
    ) -> Pin<Box<dyn Future<Output = Result<Posted, LedgerError>> + Send + 'a>> {
        Box::pin(self.do_post_payment(body))
    }

    fn void_bill<'a>(
        &'a self,
        external_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LedgerError>> + Send + 'a>> {
        Box::pin(self.do_void_bill(external_id))
    }

    fn void_payment<'a>(
        &'a self,
        external_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LedgerError>> + Send + 'a>> {
        Box::pin(self.do_void_payment(external_id))
    }

    fn attach_file<'a>(
        &'a self,
        bill_external_id: &'a str,
        name: &'a str,
        _bytes: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String, LedgerError>> + Send + 'a>> {
        Box::pin(self.do_attach_file(bill_external_id, name))
    }
}
