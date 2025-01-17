// Copyright 2024 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! This is an example of how the public 1.0 API can be used to build a proving service.
//! It's not meant to be used in production since it doesn't handle failures.
//! This is also not an optimal implementation; many performance improvements could be made.

mod plan;
mod task_mgr;
mod worker;

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::Result;
use risc0_circuit_keccak_methods::{KECCAK_ELF, KECCAK_ID};
use risc0_zkp::digest;
use risc0_zkvm::{
    sha::Digest, ApiClient, Asset, AssetRequest, CoprocessorCallback, ExecutorEnv, InnerReceipt,
    MaybePruned, ProveKeccakRequest, ProveZkrRequest, ProverOpts, Receipt, SuccinctReceipt,
    Unknown,
};

use self::{plan::Planner, task_mgr::TaskManager};

fn main() {
    prover_example();
}

struct Coprocessor {
    pub(crate) receipts: HashMap<Digest, SuccinctReceipt<Unknown>>,
}

impl Coprocessor {
    fn new() -> Self {
        Self {
            receipts: HashMap::new(),
        }
    }
}

impl CoprocessorCallback for Coprocessor {
    fn prove_zkr(&mut self, proof_request: ProveZkrRequest) -> Result<()> {
        let client = ApiClient::from_env().unwrap();
        let claim_digest = proof_request.claim_digest;
        let receipt = client.prove_zkr(proof_request, AssetRequest::Inline)?;
        self.receipts.insert(claim_digest, receipt);
        Ok(())
    }

    fn prove_keccak(&mut self, proof_request: ProveKeccakRequest) -> Result<()> {
        let client = ApiClient::from_env().unwrap();
        let receipt = client.prove_keccak(proof_request, AssetRequest::Inline)?;
        let claim_digest = match receipt.claim {
            // unknown is always pruned so if we get to this branch, something went wrong...
            MaybePruned::Value(_) => unimplemented!(),
            MaybePruned::Pruned(claim_digest) => claim_digest,
        };
        self.receipts.insert(claim_digest, receipt);
        Ok(())
    }
}

fn prover_example() {
    println!("Submitting proof request...");

    let mut task_manager = TaskManager::new();
    let mut planner = Planner::default();

    let po2 = 16;
    let claim_digest = digest!("b83c10da0c23587bf318cbcec2c2ac0260dbd6c0fa6905df639f8f6056f0d56c");
    let to_guest: (Digest, u32) = (claim_digest, po2);

    let coprocessor = Rc::new(RefCell::new(Coprocessor::new()));
    let env = ExecutorEnv::builder()
        .write(&to_guest)
        .unwrap()
        .coprocessor_callback_ref(coprocessor.clone())
        .build()
        .unwrap();

    let client = ApiClient::from_env().unwrap();
    let mut segment_idx = 0;
    let session = client
        .execute(
            &env,
            Asset::Inline(KECCAK_ELF.into()),
            AssetRequest::Inline,
            |info, segment| {
                println!("{info:?}");
                planner.enqueue_segment(segment_idx).unwrap();
                task_manager.add_segment(segment_idx, segment);
                while let Some(task) = planner.next_task() {
                    task_manager.add_task(task.clone());
                }
                segment_idx += 1;
                Ok(())
            },
        )
        .unwrap();

    planner.finish().unwrap();

    println!("Plan:");
    println!("{planner:?}");

    while let Some(task) = planner.next_task() {
        task_manager.add_task(task.clone());
    }

    let conditional_receipt = task_manager.run();

    let output = conditional_receipt
        .claim
        .as_value()
        .unwrap()
        .output
        .as_value()
        .unwrap()
        .as_ref()
        .unwrap();
    let assumptions = output.assumptions.as_value().unwrap();

    let coprocessor = coprocessor.borrow();
    let mut succinct_receipt = conditional_receipt.clone();
    for assumption in assumptions.iter() {
        let assumption = assumption.as_value().unwrap();
        println!("{assumption:?}");
        let assumption_receipt = coprocessor.receipts.get(&assumption.claim).unwrap().clone();
        let opts = ProverOpts::default();
        succinct_receipt = client
            .resolve(
                &opts,
                succinct_receipt.try_into().unwrap(),
                assumption_receipt.try_into().unwrap(),
                AssetRequest::Inline,
            )
            .unwrap();
    }

    let receipt = Receipt::new(
        InnerReceipt::Succinct(succinct_receipt),
        session.journal.bytes.clone(),
    );
    let asset = receipt.try_into().unwrap();
    client.verify(asset, KECCAK_ID).unwrap();
    println!("Receipt verified!");
}

#[test]
fn smoke_test() {
    prover_example();
}
