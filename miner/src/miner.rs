// Copyright 2015, 2016 Ethcore (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use rayon::prelude::*;
use std::sync::atomic::AtomicBool;

use util::*;
use ethcore::views::{BlockView, HeaderView};
use ethcore::client::{BlockChainClient, BlockId};
use ethcore::block::{ClosedBlock, IsBlock};
use ethcore::error::*;
use ethcore::transaction::SignedTransaction;
use super::{MinerService, MinerStatus, TransactionQueue, AccountDetails};

/// Keeps track of transactions using priority queue and holds currently mined block.
pub struct Miner {
	transaction_queue: Mutex<TransactionQueue>,

	// for sealing...
	sealing_enabled: AtomicBool,
	sealing_block_last_request: Mutex<u64>,
	sealing_work: Mutex<UsingQueue<ClosedBlock>>,
	gas_floor_target: RwLock<U256>,
	author: RwLock<Address>,
	extra_data: RwLock<Bytes>,
}

impl Default for Miner {
	fn default() -> Miner {
		Miner {
			transaction_queue: Mutex::new(TransactionQueue::new()),
			sealing_enabled: AtomicBool::new(false),
			sealing_block_last_request: Mutex::new(0),
			sealing_work: Mutex::new(UsingQueue::new(5)),
			gas_floor_target: RwLock::new(U256::zero()),
			author: RwLock::new(Address::default()),
			extra_data: RwLock::new(Vec::new()),
		}
	}
}

impl Miner {
	/// Creates new instance of miner
	pub fn new() -> Arc<Miner> {
		Arc::new(Miner::default())
	}

	/// Get the author that we will seal blocks as.
	fn author(&self) -> Address {
		*self.author.read().unwrap()
	}

	/// Get the extra_data that we will seal blocks wuth.
	fn extra_data(&self) -> Bytes {
		self.extra_data.read().unwrap().clone()
	}

	/// Get the extra_data that we will seal blocks wuth.
	fn gas_floor_target(&self) -> U256 {
		*self.gas_floor_target.read().unwrap()
	}

	/// Set the author that we will seal blocks as.
	pub fn set_author(&self, author: Address) {
		*self.author.write().unwrap() = author;
	}

	/// Set the extra_data that we will seal blocks with.
	pub fn set_extra_data(&self, extra_data: Bytes) {
		*self.extra_data.write().unwrap() = extra_data;
	}

	/// Set the gas limit we wish to target when sealing a new block.
	pub fn set_gas_floor_target(&self, target: U256) {
		*self.gas_floor_target.write().unwrap() = target;
	}

	/// Set minimal gas price of transaction to be accepted for mining.
	pub fn set_minimal_gas_price(&self, min_gas_price: U256) {
		self.transaction_queue.lock().unwrap().set_minimal_gas_price(min_gas_price);
	}

	/// Prepares new block for sealing including top transactions from queue.
	#[cfg_attr(feature="dev", allow(match_same_arms))]
	fn prepare_sealing(&self, chain: &BlockChainClient) {
		trace!(target: "miner", "prepare_sealing: entering");
		let transactions = self.transaction_queue.lock().unwrap().top_transactions();
		let mut sealing_work = self.sealing_work.lock().unwrap();
		let best_hash = chain.best_block_header().sha3();

/*
		// check to see if last ClosedBlock in would_seals is actually same parent block.
		// if so
		//   duplicate, re-open and push any new transactions.
		//   if at least one was pushed successfully, close and enqueue new ClosedBlock;
		//   otherwise, leave everything alone.
		// otherwise, author a fresh block.
*/

		let (b, invalid_transactions) = match sealing_work.pop_if(|b| b.block().fields().header.parent_hash() == &best_hash) {
			Some(old_block) => {
				trace!(target: "miner", "Already have previous work; updating and returning");
				// add transactions to old_block
				let e = chain.engine();
				let mut invalid_transactions = HashSet::new();
				let mut block = old_block.reopen(e);
				let block_number = block.block().fields().header.number();

				// TODO: push new uncles, too.
				// TODO: refactor with chain.prepare_sealing
				for tx in transactions {
					let hash = tx.hash();
					let res = block.push_transaction(tx, None);
					match res {
						Err(Error::Execution(ExecutionError::BlockGasLimitReached { gas_limit, gas_used, .. })) => {
							trace!(target: "miner", "Skipping adding transaction to block because of gas limit: {:?}", hash);
							// Exit early if gas left is smaller then min_tx_gas
							let min_tx_gas: U256 = x!(21000);	// TODO: figure this out properly.
							if gas_limit - gas_used < min_tx_gas {
								break;
							}
						},
						Err(Error::Transaction(TransactionError::AlreadyImported)) => {}	// already have transaction - ignore
						Err(e) => {
							invalid_transactions.insert(hash);
							trace!(target: "miner",
								   "Error adding transaction to block: number={}. transaction_hash={:?}, Error: {:?}",
								   block_number, hash, e);
						},
						_ => {}	// imported ok
					}
				}
				(Some(block.close()), invalid_transactions)
			}
			None => {
				// block not found - create it.
				trace!(target: "miner", "No existing work - making new block");
				chain.prepare_sealing(
					self.author(),
					self.gas_floor_target(),
					self.extra_data(),
					transactions,
				)
			}
		};
		let mut queue = self.transaction_queue.lock().unwrap();
		queue.remove_all(
			&invalid_transactions.into_iter().collect::<Vec<H256>>(),
			|a: &Address| AccountDetails {
				nonce: chain.nonce(a),
				balance: chain.balance(a),
			}
		);
		if let Some(block) = b {
			if sealing_work.peek_last_ref().map_or(true, |pb| pb.block().fields().header.hash() != block.block().fields().header.hash()) {
				trace!(target: "miner", "Pushing a new, refreshed or borrowed pending {}...", block.block().fields().header.hash());
				sealing_work.push(block);
			}
		}
		trace!(target: "miner", "prepare_sealing: leaving (last={:?})", sealing_work.peek_last_ref().map(|b| b.block().fields().header.hash()));
	}

	fn update_gas_limit(&self, chain: &BlockChainClient) {
		let gas_limit = HeaderView::new(&chain.best_block_header()).gas_limit();
		let mut queue = self.transaction_queue.lock().unwrap();
		queue.set_gas_limit(gas_limit);
	}
}

const SEALING_TIMEOUT_IN_BLOCKS : u64 = 5;

impl MinerService for Miner {

	fn clear_and_reset(&self, chain: &BlockChainClient) {
		self.transaction_queue.lock().unwrap().clear();
		self.update_sealing(chain);
	}

	fn status(&self) -> MinerStatus {
		let status = self.transaction_queue.lock().unwrap().status();
		let sealing_work = self.sealing_work.lock().unwrap();
		MinerStatus {
			transactions_in_pending_queue: status.pending,
			transactions_in_future_queue: status.future,
			transactions_in_pending_block: sealing_work.peek_last_ref().map_or(0, |b| b.transactions().len()),
		}
	}

	fn sensible_gas_price(&self) -> U256 {
		// 10% above our minimum.
		*self.transaction_queue.lock().unwrap().minimal_gas_price() * x!(110) / x!(100)
	}

	fn author(&self) -> Address {
		*self.author.read().unwrap()
	}

	fn extra_data(&self) -> Bytes {
		self.extra_data.read().unwrap().clone()
	}

	fn import_transactions<T>(&self, transactions: Vec<SignedTransaction>, fetch_account: T) -> Vec<Result<(), Error>>
		where T: Fn(&Address) -> AccountDetails {
		let mut transaction_queue = self.transaction_queue.lock().unwrap();
		transaction_queue.add_all(transactions, fetch_account)
	}

	fn pending_transactions_hashes(&self) -> Vec<H256> {
		let transaction_queue = self.transaction_queue.lock().unwrap();
		transaction_queue.pending_hashes()
	}

	fn transaction(&self, hash: &H256) -> Option<SignedTransaction> {
		let queue = self.transaction_queue.lock().unwrap();
		queue.find(hash)
	}

	fn pending_transactions(&self) -> Vec<SignedTransaction> {
		let queue = self.transaction_queue.lock().unwrap();
		queue.top_transactions()
	}

	fn last_nonce(&self, address: &Address) -> Option<U256> {
		self.transaction_queue.lock().unwrap().last_nonce(address)
	}

	fn update_sealing(&self, chain: &BlockChainClient) {
		if self.sealing_enabled.load(atomic::Ordering::Relaxed) {
			let current_no = chain.chain_info().best_block_number;
			let last_request = *self.sealing_block_last_request.lock().unwrap();
			let should_disable_sealing = current_no > last_request && current_no - last_request > SEALING_TIMEOUT_IN_BLOCKS;

			if should_disable_sealing {
				trace!(target: "miner", "Miner sleeping (current {}, last {})", current_no, last_request);
				self.sealing_enabled.store(false, atomic::Ordering::Relaxed);
				self.sealing_work.lock().unwrap().reset();
			} else if self.sealing_enabled.load(atomic::Ordering::Relaxed) {
				self.prepare_sealing(chain);
			}
		}
	}

	fn map_sealing_work<F, T>(&self, chain: &BlockChainClient, f: F) -> Option<T> where F: FnOnce(&ClosedBlock) -> T {
		trace!(target: "miner", "map_sealing_work: entering");
		let have_work = self.sealing_work.lock().unwrap().peek_last_ref().is_some();
		trace!(target: "miner", "map_sealing_work: have_work={}", have_work);
		if !have_work {
			self.sealing_enabled.store(true, atomic::Ordering::Relaxed);
			self.prepare_sealing(chain);
		}
		let mut sealing_block_last_request = self.sealing_block_last_request.lock().unwrap();
		let best_number = chain.chain_info().best_block_number;
		if *sealing_block_last_request != best_number {
			trace!(target: "miner", "map_sealing_work: Miner received request (was {}, now {}) - waking up.", *sealing_block_last_request, best_number);
			*sealing_block_last_request = best_number;
		}

		let mut sealing_work = self.sealing_work.lock().unwrap();
		let ret = sealing_work.use_last_ref();
		trace!(target: "miner", "map_sealing_work: leaving use_last_ref={:?}", ret.as_ref().map(|b| b.block().fields().header.hash()));
		ret.map(f)
	}

	fn submit_seal(&self, chain: &BlockChainClient, pow_hash: H256, seal: Vec<Bytes>) -> Result<(), Error> {
		if let Some(b) = self.sealing_work.lock().unwrap().take_used_if(|b| &b.hash() == &pow_hash) {
			match chain.try_seal(b.lock(), seal) {
				Err(_) => {
					Err(Error::PowInvalid)
				}
				Ok(sealed) => {
					// TODO: commit DB from `sealed.drain` and make a VerifiedBlock to skip running the transactions twice.
					try!(chain.import_block(sealed.rlp_bytes()));
					Ok(())
				}
			}
		} else {
			Err(Error::PowHashInvalid)
		}
	}

	fn chain_new_blocks(&self, chain: &BlockChainClient, imported: &[H256], invalid: &[H256], enacted: &[H256], retracted: &[H256]) {
		fn fetch_transactions(chain: &BlockChainClient, hash: &H256) -> Vec<SignedTransaction> {
			let block = chain
				.block(BlockId::Hash(*hash))
				// Client should send message after commit to db and inserting to chain.
				.expect("Expected in-chain blocks.");
			let block = BlockView::new(&block);
			block.transactions()
		}

		// First update gas limit in transaction queue
		self.update_gas_limit(chain);

		// Then import all transactions...
		{
			let out_of_chain = retracted
				.par_iter()
				.map(|h| fetch_transactions(chain, h));
			out_of_chain.for_each(|txs| {
				// populate sender
				for tx in &txs {
					let _sender = tx.sender();
				}
				let mut transaction_queue = self.transaction_queue.lock().unwrap();
				let _ = transaction_queue.add_all(txs, |a| AccountDetails {
					nonce: chain.nonce(a),
					balance: chain.balance(a)
				});
			});
		}

		// ...and after that remove old ones
		{
			let in_chain = {
				let mut in_chain = HashSet::new();
				in_chain.extend(imported);
				in_chain.extend(enacted);
				in_chain.extend(invalid);
				in_chain
					.into_iter()
					.collect::<Vec<H256>>()
			};

			let in_chain = in_chain
				.par_iter()
				.map(|h: &H256| fetch_transactions(chain, h));

			in_chain.for_each(|txs| {
				let hashes = txs.iter().map(|tx| tx.hash()).collect::<Vec<H256>>();
				let mut transaction_queue = self.transaction_queue.lock().unwrap();
				transaction_queue.remove_all(&hashes, |a| AccountDetails {
					nonce: chain.nonce(a),
					balance: chain.balance(a)
				});
			});
		}

		self.update_sealing(chain);
	}
}

#[cfg(test)]
mod tests {

	use MinerService;
	use super::{Miner};
	use util::*;
	use ethcore::client::{TestBlockChainClient, EachBlockWith};
	use ethcore::block::*;

	// TODO [ToDr] To uncomment when TestBlockChainClient can actually return a ClosedBlock.
	#[ignore]
	#[test]
	fn should_prepare_block_to_seal() {
		// given
		let client = TestBlockChainClient::default();
		let miner = Miner::default();

		// when
		let sealing_work = miner.map_sealing_work(&client, |_| ());
		assert!(sealing_work.is_some(), "Expected closed block");
	}

	#[ignore]
	#[test]
	fn should_still_work_after_a_couple_of_blocks() {
		// given
		let client = TestBlockChainClient::default();
		let miner = Miner::default();

		let res = miner.map_sealing_work(&client, |b| b.block().fields().header.hash());
		assert!(res.is_some());
		assert!(miner.submit_seal(&client, res.unwrap(), vec![]).is_ok());

		// two more blocks mined, work requested.
		client.add_blocks(1, EachBlockWith::Uncle);
		miner.map_sealing_work(&client, |b| b.block().fields().header.hash());

		client.add_blocks(1, EachBlockWith::Uncle);
		miner.map_sealing_work(&client, |b| b.block().fields().header.hash());

		// solution to original work submitted.
		assert!(miner.submit_seal(&client, res.unwrap(), vec![]).is_ok());
	}
}
