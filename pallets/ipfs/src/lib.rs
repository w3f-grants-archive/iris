// This file is part of Iris.
//
// Copyright (C) 2022 Ideal Labs.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! # IPFS Pallet
//!
//! @author driemworks
//! 
//! ## Description 
//! 
//! This pallet contains the core integration and configuration
//! with an external IPFS instance, as interfaced with via the IPFS 
//! RPC endpoints.
//! 

#![cfg_attr(not(feature = "std"), no_std)]

mod mock;
mod tests;

use frame_support::{
	ensure,
	pallet_prelude::*,
	traits::{
		EstimateNextSessionRotation, Get,
		ValidatorSet, ValidatorSetWithIdentification,
	},
};
use log;
use scale_info::TypeInfo;
pub use pallet::*;
use sp_runtime::traits::{Convert, Zero};
use sp_staking::offence::{Offence, OffenceError, ReportOffence};
use sp_std::{
	collections::{btree_set::BTreeSet, btree_map::BTreeMap},
	str,
	vec::Vec,
	prelude::*
};
use sp_core::{
    offchain::{
        OpaqueMultiaddr, StorageKind,
    },
	crypto::KeyTypeId,
};
use frame_system::{
	self as system, 
	ensure_signed,
	offchain::{
		SendSignedTransaction,
		Signer,
	}
};
use sp_runtime::{
	// offchain::ipfs,
	offchain::http,
	traits::StaticLookup,
};
use pallet_data_assets::DataCommand;

pub const LOG_TARGET: &'static str = "runtime::proxy";
// TODO: should a new KeyTypeId be defined? e.g. b"iris"
pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"aura");

pub mod crypto {
	// use crate::KEY_TYPE;
	use sp_core::crypto::KeyTypeId;
	use sp_core::sr25519::Signature as Sr25519Signature;
	use sp_runtime::app_crypto::{app_crypto, sr25519};
	use sp_runtime::{traits::Verify, MultiSignature, MultiSigner};
	use sp_std::convert::TryFrom;

	pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"aura");

	app_crypto!(sr25519, KEY_TYPE);

	pub struct TestAuthId;
	// implemented for runtime
	impl frame_system::offchain::AppCrypto<MultiSigner, MultiSignature> for TestAuthId {
		type RuntimeAppPublic = Public;
		type GenericSignature = sp_core::sr25519::Signature;
		type GenericPublic = sp_core::sr25519::Public;
	}

	// implemented for mock runtime in test
	impl frame_system::offchain::AppCrypto<<Sr25519Signature as Verify>::Signer, Sr25519Signature>
		for TestAuthId
	{
		type RuntimeAppPublic = Public;
		type GenericSignature = sp_core::sr25519::Signature;
		type GenericPublic = sp_core::sr25519::Public;
	}
}

/// Counter for the number of eras that have passed.
pub type EraIndex = u32;
/// counter for the number of "reward" points earned by a given storage provider
pub type RewardPoint = u32;

/// Reward points for storage providers of some specific assest id during an era.
#[derive(PartialEq, Encode, Decode, Default, RuntimeDebug, TypeInfo)]
pub struct EraRewardPoints<AccountId> {
	/// the total number of points
	total: RewardPoint,
	/// the reward points for individual validators, sum(i.rewardPoint in individual) = total
	individual: BTreeMap<AccountId, RewardPoint>,
}

/// Information regarding the active era (era in used in session).
#[derive(Encode, Decode, RuntimeDebug, TypeInfo)]
pub struct ActiveEraInfo {
	/// Index of era.
	pub index: EraIndex,
	/// Moment of start expressed as millisecond from `$UNIX_EPOCH`.
	///
	/// Start can be none if start hasn't been set for the era yet,
	/// Start is set on the first on_finalize of the era to guarantee usage of `Time`.
	start: Option<u64>,
}

#[frame_support::pallet]
pub mod pallet {
	use super::*;
	use frame_system::{
		pallet_prelude::*,
		offchain::{
			AppCrypto,
			CreateSignedTransaction,
		}
	};

	/// Configure the pallet by specifying the parameters and types on which it
	/// depends.
	/// TODO: reafactor? lots of tightly coupled pallets here, there must  
	/// be a better way to go about this
	#[pallet::config]
	pub trait Config: CreateSignedTransaction<Call<Self>> +
					  frame_system::Config +
					  pallet_data_assets::Config + 
					  pallet_authorization::Config
	{
		/// The Event type.
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		/// the overarching call type
		type Call: From<Call<Self>>;
		/// Origin for adding or removing a validator.
		type AddRemoveOrigin: EnsureOrigin<Self::Origin>;
		/// Minimum number of validators to leave in the validator set during
		/// auto removal.
		type MinAuthorities: Get<u32>;
		/// the maximum number of session that a node can earn less than MinEraRewardPoints before suspension
		type MaxDeadSession: Get<u32>;
		/// the authority id used for sending signed txs
        type AuthorityId: AppCrypto<Self::Public, Self::Signature>;
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	#[pallet::without_storage_info]
	pub struct Pallet<T>(_);

    /// map the ipfs public key to a list of multiaddresses
    #[pallet::storage]
    #[pallet::getter(fn bootstrap_nodes)]
    pub(super) type BootstrapNodes<T: Config> = StorageMap<
        _, Blake2_128Concat, Vec<u8>, Vec<OpaqueMultiaddr>, ValueQuery,
    >;

	/// map substrate public key to ipfs public key
	#[pallet::storage]
	#[pallet::getter(fn substrate_ipfs_bridge)]
	pub(super) type SubstrateIpfsBridge<T: Config> = StorageMap<
		_, Blake2_128Concat, T::AccountId, Vec<u8>, ValueQuery,
	>;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// New validator addition initiated. Effective in ~2 sessions.
		StakeSuccessful(T::AccountId),
	}

	
	#[pallet::validate_unsigned]
	impl<T: Config> ValidateUnsigned for Pallet<T> {
		type Call = Call<T>;

		/// Validate unsigned call to this module.
		///
		fn validate_unsigned(_source: TransactionSource, call: &Self::Call) -> TransactionValidity {
			if let Call::submit_rpc_ready { .. } = call {
				Self::validate_transaction_parameters()
			} else if let Call::submit_ipfs_identity{ .. } = call {
				Self::validate_transaction_parameters()
			} else {
				InvalidTransaction::Call.into()
			}
		}
	}

	// Errors inform users that something went wrong.
	#[pallet::error]
	pub enum Error<T> {
		/// could not build the ipfs request
		CantCreateRequest,
		/// the request to IPFS timed out
		RequestTimeout,
		/// the request to IPFS failed
		RequestFailed,
		/// the specified asset id does not correspond to any owned content
		NoSuchOwnedContent,
		/// the nodes balance is insufficient to complete this operation
		InsufficientBalance,
		/// the node is already a candidate for some storage pool
		AlreadyACandidate,
		/// the node has already pinned the CID
		AlreadyPinned,
		/// the node is not a candidate storage provider for some asset id
		NotACandidate,
		InvalidMultiaddress,
		InvalidCID,
		IpfsError,
	}

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn offchain_worker(block_number: T::BlockNumber) {
			// every 5 blocks
			if block_number % 5u32.into() == 0u32.into() {
				if let Err(e) = Self::connection_housekeeping() {
					log::error!("Encountered an error while processing data requests: {:?}", e);
				}
			}
			// handle data requests each block
			if let Err(e) = Self::prcoess_ingestion_requests() {
				log::error!("Encountered an error while processing data requests: {:?}", e);
			}

			if let Err(e) = Self::process_ejection_queue() {
				log::error!("Encountered an error while processing data requests: {:?}", e);
			}
		}
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {

		/// TODO: I really need to address the fact that this is callable by anyone
		/// Someone could randomly make an asset class on your behalf, making you the admin
		/// 
		/// should only be called by offchain workers... how to ensure this?
        /// submits IPFS results on chain and creates new ticket config in runtime storage
        ///
        /// * `admin`: The admin account
        /// * `cid`: The cid generated by the OCW
        /// * `id`: The AssetId (passed through from the create_storage_asset call)
        /// * `balance`: The balance (passed through from the create_storage_asset call)
        ///
        #[pallet::weight(100)]
        pub fn submit_ipfs_add_results(
            origin: OriginFor<T>,
            admin: <T::Lookup as StaticLookup>::Source,
            cid: Vec<u8>,
            id: T::AssetId,
            balance: T::Balance,
			dataspace_id: T::AssetId,
        ) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let new_origin = system::RawOrigin::Signed(who.clone()).into();
			// creates the asset class
            <pallet_data_assets::Pallet<T>>::submit_ipfs_add_results(
				new_origin,
				admin,
				cid,
				dataspace_id,
				id,
				balance,
			)?;
            Ok(())
        }

        /// Should only be callable by OCWs (TODO)
        /// Submit the results of an `ipfs identity` call to be stored on chain
        ///
        /// * origin: a validator node
        /// * public_key: The IPFS node's public key
        /// * multiaddresses: A vector of multiaddresses associate with the public key
        ///
        #[pallet::weight(100)]
        pub fn submit_ipfs_identity(
            origin: OriginFor<T>,
            public_key: Vec<u8>,
            multiaddresses: Vec<OpaqueMultiaddr>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            <BootstrapNodes::<T>>::insert(public_key.clone(), multiaddresses.clone());
            <SubstrateIpfsBridge::<T>>::insert(who.clone(), public_key.clone());
			// Self::deposit_event(Event::PublishedIdentity(who.clone()));
            Ok(())
        }

		/// should only be callable by validator nodes (TODO)
		/// 
		/// * `asset_id`: The asset id corresponding to the data that was pinned
		/// * `pinner': The node claiming to have pinned the data
		/// 
		#[pallet::weight(100)]
		pub fn submit_ipfs_pin_result(
			origin: OriginFor<T>,
			asset_id: T::AssetId,
			pinner: T::AccountId,
		) -> DispatchResult {
			// let _who = ensure_signed(origin)?;
			// // verify they are a candidate storage provider
			// let candidate_storage_providers = <QueuedStorageProviders::<T>>::get(asset_id.clone());
			// ensure!(candidate_storage_providers.contains(&pinner), Error::<T>::NotACandidate);
			// // verify not already pinning the content
			// let current_pinners = <Pinners::<T>>::get(asset_id.clone());
			// ensure!(!current_pinners.contains(&pinner), Error::<T>::AlreadyPinned);
			// // TODO: we need a better scheme for *generating* pool ids -> should always be unique (cid + owner maybe?)
			// <Pinners<T>>::mutate(asset_id.clone(), |p| {
			// 	p.push(pinner.clone());
			// });
			// // award point to pinner
			// if let Some(active_era) = ActiveEra::<T>::get() {
			// 	SessionParticipation::<T>::mutate(active_era.clone(), |p| {
			// 		p.push(pinner.clone());
			// 	});
			// 	// WIP: TODO
			// 	// <ErasRewardPoints<T>>::mutate(active_era, asset_id, |era_rewards| {
			// 	// 	*era_rewards.unwrap().individual.entry(pinner.clone()).or_default() += 1;
			// 	// 	era_rewards.unwrap().total += 1;
			// 	// });
			// }
			Ok(())
		}

        /// Should only be callable by OCWs (TODO)
        /// Submit the results onchain to notify a beneficiary that their data is available: TODO: how to safely share host? spam protection on rpc endpoints?
        ///
        /// * `beneficiary`: The account that requested the data
        /// * `host`: The node's host where the data has been made available (RPC endpoint)
        ///
        #[pallet::weight(100)]
        pub fn submit_rpc_ready(
            _origin: OriginFor<T>,
			_asset_id: T::AssetId,
        ) -> DispatchResult {
            // ensure_signed(origin)?;
			// WIP: TODO
			// if let Some(active_era) = ActiveEra::<T>::get() {
			// 	<ErasRewardPoints<T>>::mutate(active_era.clone(), asset_id.clone(), |era_rewards| {
			// 		// reward all active storage providers
			// 		for k in StorageProviders::<T>::get(asset_id.clone()).into_iter() {
			// 			SessionParticipation::<T>::mutate(active_era.clone(), |p| {
			// 				p.push(k.clone());
			// 			});
			// 			*era_rewards.unwrap().individual.entry(k.clone()).or_default() += 1;
			// 			era_rewards.unwrap().total += 1;
			// 		}
			// 	});
			// }
            Ok(())
        }
	}
}

impl<T: Config> Pallet<T> {

	fn validate_transaction_parameters() -> TransactionValidity {
		ValidTransaction::with_tag_prefix("iris")
			.longevity(5)
			.propagate(true)
			.build()
	}
	
	/// manage connection to the iris ipfs swarm
    ///
    /// If the node is already a bootstrap node, do nothing. Otherwise submits a signed tx 
    /// containing the public key and multiaddresses of the embedded ipfs node.
    /// 
    /// Returns an error if communication with the embedded IPFS fails
    fn connection_housekeeping() -> Result<(), Error<T>> {
        Ok(())
    }

	fn process_ejection_queue() -> Result<(), Error<T>> {
		let data_retrieval_queue = <pallet_authorization::Pallet<T>>::data_retrieval_queue();
		let len = data_retrieval_queue.len();
		if len != 0 {
			log::info!("{} entr{} in the data retrieval queue", len, if len == 1 { "y" } else { "ies" });
		}
		for cmd in data_retrieval_queue.into_iter() {
			match cmd {
				DataCommand::CatBytes(_requestor, _owner, asset_id) => {
					match <pallet_data_assets::Pallet<T>>::metadata(asset_id.clone()) {
						Some(metadata) => {
							let cid = metadata.cid;
							// let res = Self::ipfs_cat(&cid)?;
							// let data = res.body().collect::<Vec<u8>>();
							// log::info!("IPFS: Fetched data from IPFS.");
							// add to offchain index
							sp_io::offchain::local_storage_set(
								StorageKind::PERSISTENT,
								&cid,
								&cid,
							);

							let signer = Signer::<T, T::AuthorityId>::all_accounts();
							if !signer.can_sign() {
								log::error!(
									"No local accounts available. Consider adding one via `author_insertKey` RPC.",
								);
							}

							let results = signer.send_signed_transaction(|_account| { 
								Call::submit_rpc_ready {
									asset_id: asset_id.clone(),
								}
							});
					
							for (_, res) in &results {
								match res {
									Ok(()) => log::info!("Submitted ipfs results"),
									Err(e) => log::error!("Failed to submit transaction: {:?}",  e),
								}
							}
						},
						None => {
							return Ok(());
						}
					}
				},
				_ => {

				}
			}
		}

		Ok(())
	}

	/// process any requests in the IngestionQueue
	/// TODO: This needs some *major* refactoring
    fn prcoess_ingestion_requests() -> Result<(), Error<T>> {
		let ingestion_queue = <pallet_data_assets::Pallet<T>>::ingestion_queue();
		let len = ingestion_queue.len();
		if len != 0 {
			log::info!("{} entr{} in the data queue", len, if len == 1 { "y" } else { "ies" });
		}
		for cmd in ingestion_queue.into_iter() {
			match cmd {
				DataCommand::AddBytes(_addr, cid, admin, id, balance, dataspace_id) => {
					if sp_io::offchain::is_validator() {
						// Self::ipfs_connect(&addr);
						// Self::ipfs_get(&cid);
						// Self::ipfs_disconnect(&addr);

						let signer = Signer::<T, T::AuthorityId>::all_accounts();
						if !signer.can_sign() {
							log::error!(
								"No local accounts available. Consider adding one via `author_insertKey` RPC.",
							);
						}
						let results = signer.send_signed_transaction(|_account| { 
							Call::submit_ipfs_add_results{
								admin: admin.clone(),
								cid: cid.clone(),
								dataspace_id: dataspace_id.clone(),
								id: id.clone(),
								balance: balance.clone(),
							}
						});
				
						for (_, res) in &results {
							match res {
								Ok(()) => log::info!("Submitted results"),
								Err(e) => log::error!("Failed to submit transaction: {:?}",  e),
							}
						}
					}
				},
				_ => {
					// do nothing
				}
			}
		}

        Ok(())
    }

	/*
	IPFS commands: This should ultimately be moved to it's own file
	*/
	/// ipfs swarm connect
	fn ipfs_connect(multiaddress: &Vec<u8>) -> Result<(), Error<T>> {
		match str::from_utf8(multiaddress) {
			Ok(maddr) => {
				let mut endpoint = "http://127.0.0.1:5001/api/v0/swarm/connect?arg=".to_owned();
				endpoint.push_str(maddr);
				Self::ipfs_post_request(&endpoint).map_err(|_| Error::<T>::IpfsError).unwrap();
				return Ok(());
			},
			Err(_e) => {
				return Err(Error::<T>::InvalidMultiaddress);
			}
		}
	}

	/// ipfs swarm disconnect
	fn ipfs_disconnect(multiaddress: &Vec<u8>) -> Result<(), Error<T>> {
		match str::from_utf8(multiaddress) {
			Ok(maddr) => {
				let mut endpoint = "http://127.0.0.1:5001/api/v0/swarm/disconnect?arg=".to_owned();
				endpoint.push_str(maddr);
				Self::ipfs_post_request(&endpoint).map_err(|_| Error::<T>::IpfsError).unwrap();
				return Ok(());
			},
			Err(_e) => {
				return Err(Error::<T>::InvalidMultiaddress);
			}
		}
	}

	/// ipfs get <CID>
	fn ipfs_get(cid: &Vec<u8>) -> Result<(), Error<T>> {
		match str::from_utf8(cid) {
			Ok(cid_string) => {
				let mut endpoint = "http://127.0.0.1:5001/api/v0/swarm/disconnect?arg=".to_owned();
				endpoint.push_str(cid_string);
				Self::ipfs_post_request(&endpoint).map_err(|_| Error::<T>::IpfsError).unwrap();
				return Ok(());
			},
			Err(_e) => {
				return Err(Error::<T>::InvalidCID);
			}
		}
	}

	/// ipfs cat <CID>
	fn ipfs_cat(cid: &Vec<u8>) -> Result<http::Response, Error<T>> {
		match str::from_utf8(cid) {
			Ok(cid_string) => {
				let mut endpoint = "http://127.0.0.1:5001/api/v0/cat?arg=".to_owned();
				endpoint.push_str(cid_string);
				let res = Self::ipfs_post_request(&endpoint).map_err(|_| Error::<T>::IpfsError).ok();
				return Ok(res.unwrap());
			},
			Err(_e) => {
				return Err(Error::<T>::InvalidCID);
			}
		}
	}

	/// Make an http post request to IPFS
	/// 
	/// * `endpoint`: The IPFS endpoint to invoke
	fn ipfs_post_request(endpoint: &str) -> Result<http::Response, http::Error> {
		let pending = http::Request::default()
					.method(http::Method::Post)
					.url(endpoint)
					.body(vec![b""])
					.send()
					.unwrap();
		let response = pending.wait().unwrap();
		if response.code != 200 {
			log::warn!("Unexpected status code: {}", response.code);
			return Err(http::Error::Unknown)
		}
		Ok(response)
	}
}
