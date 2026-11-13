//! An in-memory ledger for tests. It records each call and returns the next
//! scripted result, or a new id when the script is empty.
//!
//! A key that was already accepted returns that id only when the body matches.
//! A different body is a conflict. Voiding an unknown id is a conflict;
//! voiding an already-voided id succeeds without another reversal. The same
//! attached file name and bytes return the same file id.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::{BillBody, Code, Ledger, LedgerError, PaymentBody, Posted, body_hash};

#[derive(Clone)]
struct Remembered {
    hash: String,
    id: String,
    voided: bool,
}

#[derive(Clone, Default)]
struct Inner {
    script: VecDeque<Result<(), LedgerError>>,
    calls: Vec<String>,
    ids: HashMap<String, Remembered>,
    /// `(bill external id, file name)` → `(file id, bytes hash)`.
    files: HashMap<(String, String), (String, String)>,
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

    /// Holds a place so a later [`Self::script_err`] hits a later call.
    #[cfg(test)]
    pub(crate) fn script_ok(&self) {
        self.inner
            .lock()
            .expect("fake ledger")
            .script
            .push_back(Ok(()));
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

    /// Same key and same body return the first id. A different body is refused
    /// and does not return that id. A scripted error is taken before this runs,
    /// so a refused call does not store the body.
    fn remember(
        &self,
        key: &str,
        hash: &str,
        mint: &mut dyn FnMut(&mut Inner) -> String,
    ) -> Result<String, LedgerError> {
        let mut inner = self.inner.lock().expect("fake ledger");
        if let Some(found) = inner.ids.get(key) {
            if found.voided {
                return Err(LedgerError::new(
                    Code::Conflict,
                    "this ledger record has been voided",
                ));
            }
            if found.hash == hash {
                return Ok(found.id.clone());
            }
            return Err(LedgerError::new(
                Code::Conflict,
                "this key was already posted with a different body",
            ));
        }
        let id = mint(&mut inner);
        inner.ids.insert(
            key.to_owned(),
            Remembered {
                hash: hash.to_owned(),
                id: id.clone(),
                voided: false,
            },
        );
        Ok(id)
    }

    fn mark_voided(&self, external_id: &str) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().expect("fake ledger");
        let Some(found) = inner.ids.values_mut().find(|row| row.id == external_id) else {
            return Err(LedgerError::new(
                Code::Conflict,
                format!("unknown ledger id {external_id}"),
            ));
        };
        found.voided = true;
        Ok(())
    }

    async fn do_post_bill(&self, body: &BillBody) -> Result<Posted, LedgerError> {
        self.take(format!("post_bill {}", body.external_key))?;
        let hash = body_hash(body);
        let key = body.external_key.clone();
        let external_id = self.remember(&key, &hash, &mut |inner| {
            inner.next_bill += 1;
            format!("fake-bill-{}", inner.next_bill)
        })?;
        Ok(Posted {
            external_id,
            external_key: key,
        })
    }

    async fn do_post_payment(&self, body: &PaymentBody) -> Result<Posted, LedgerError> {
        self.take(format!("post_payment {}", body.external_key))?;
        let hash = body_hash(body);
        let key = body.external_key.clone();
        let external_id = self.remember(&key, &hash, &mut |inner| {
            inner.next_payment += 1;
            format!("fake-payment-{}", inner.next_payment)
        })?;
        Ok(Posted {
            external_id,
            external_key: key,
        })
    }

    async fn do_void_bill(&self, external_id: &str) -> Result<(), LedgerError> {
        self.take(format!("void_bill {external_id}"))?;
        self.mark_voided(external_id)
    }

    async fn do_void_payment(&self, external_id: &str) -> Result<(), LedgerError> {
        self.take(format!("void_payment {external_id}"))?;
        self.mark_voided(external_id)
    }

    async fn do_attach_file(
        &self,
        bill_external_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<String, LedgerError> {
        self.take(format!("attach_file {bill_external_id} {name}"))?;
        let digest = hex::encode(Sha256::digest(bytes));
        let mut inner = self.inner.lock().expect("fake ledger");
        let key = (bill_external_id.to_owned(), name.to_owned());
        if let Some((id, hash)) = inner.files.get(&key) {
            if hash == &digest {
                return Ok(id.clone());
            }
            return Err(LedgerError::new(
                Code::Conflict,
                format!("file {name} was already attached with different bytes"),
            ));
        }
        inner.next_file += 1;
        let id = format!("fake-file-{}", inner.next_file);
        inner.files.insert(key, (id.clone(), digest));
        Ok(id)
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

    fn set_document_link<'a>(
        &'a self,
        bill_external_key: &'a str,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LedgerError>> + Send + 'a>> {
        Box::pin(async move {
            self.take(format!("set_document_link {bill_external_key} {url}"))?;
            Ok(())
        })
    }

    fn set_bank_reference<'a>(
        &'a self,
        payment_external_key: &'a str,
        reference: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LedgerError>> + Send + 'a>> {
        Box::pin(async move {
            self.take(format!(
                "set_bank_reference {payment_external_key} {reference}"
            ))?;
            Ok(())
        })
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
        bytes: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String, LedgerError>> + Send + 'a>> {
        Box::pin(self.do_attach_file(bill_external_id, name, bytes))
    }
}
