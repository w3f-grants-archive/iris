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
#![cfg_attr(not(feature = "std"), no_std)]

//! # Iris Assets Pallet
//!
//! ## Overview
//!
//! ### Goals
//! The Iris module provides functionality for creation and 
//! management of storage assets and access management
//! 
//! ### Dispatchable Functions 
//!
//! #### Permissionless functions
//! * create_storage_asset
//!
//! #### Permissioned Functions
//! * mint_tickets
//!

use scale_info::TypeInfo;
use codec::{Encode, Decode};
use frame_support::ensure;
use frame_system::{
    self as system, ensure_signed,
};

use sp_core::{
    offchain::{StorageKind},
    Bytes,
};

use sp_runtime::{
    RuntimeDebug,
    traits::StaticLookup,
};
use sp_std::{
    prelude::*,
};

use core::convert::TryInto;
use iris_primitives::IngestionCommand;

// #[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
// pub struct IngestionCommand<AccountId, AssetId, OccId, Balance> {
//     pub owner: AccountId,
//     /// the desired asset id
//     pub asset_id: AssetId,
//     /// the dataspace id to associate the asset with
//     pub dataspace_id: AssetId,
//     /// the id of the data within the offchain client
//     pub occ_id: OccId,
//     /// a 'self-reported' estimated size of data to be transferred
//     /// the true data size can only be known after querying the OCC within the OCW
//     pub estimated_size_gb: u128,
//     /// the balance used to create an asset class and pay a proxy node
//     pub balance: Balance,
// }

#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub struct EjectionCommand {

}

#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub enum DataCommand<LookupSource, AssetId, Balance, AccountId> {
    /// (ipfs_address, cid, requesting node address, asset id, balance, dataspace_id)
    AddBytes(Vec<u8>, Vec<u8>, LookupSource, AssetId, Balance, AssetId),
    /// (requestor, owner, assetid)
    CatBytes(AccountId, AccountId, AssetId),
    /// (node, assetid, CID)
    PinCID(AccountId, AssetId, Vec<u8>),
    /// asset id, lsit of dataspace ids
    AddToDataSpace(AssetId, AssetId),
}

/// struct to store metadata of an asset class
#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub struct AssetMetadata {
    /// the cid of some data
    pub cid: Vec<u8>,
    /// the occ id of the data
    pub occ_id: Vec<u8>,
}

pub use pallet::*;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;

#[frame_support::pallet]
pub mod pallet {
    use super::*;
	use frame_support::{dispatch::DispatchResult, pallet_prelude::*};
	use frame_system::{
        pallet_prelude::*,
    };
	use sp_std::{
        str,
    };

	#[pallet::config]
    /// the module configuration trait
	pub trait Config: frame_system::Config + pallet_assets::Config
    {
        /// The overarching event type
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
        /// the overarching call type
	    type Call: From<Call<Self>>;
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
    #[pallet::without_storage_info]
	pub struct Pallet<T>(_);

    /// A queue of data to publish or obtain on IPFS.
    /// Commands are processed by offchain workers (of validators) in the iris-session pallet
	#[pallet::storage]
    #[pallet::getter(fn ingestion_queue)]
	pub(super) type IngestionQueue<T: Config> = StorageValue<
        _, Vec<IngestionCommand<T::AccountId, T::Balance>>, ValueQuery,
    >;

	#[pallet::storage]
    #[pallet::getter(fn ejection_queue)]
	pub(super) type EjectionQueue<T: Config> = StorageValue<
        _,
        Vec<DataCommand<
            <T::Lookup as StaticLookup>::Source, 
            T::AssetId,
            T::Balance,
            T::AccountId>
        >,
        ValueQuery,
    >;

    /// A collection of asset ids
    /// TODO: currently allows customized asset ids but in the future
    /// we can use this to dynamically generate unique asset ids for content
    #[pallet::storage]
    #[pallet::getter(fn asset_ids)]
    pub(super) type AssetIds<T: Config> = StorageValue<
        _,
        Vec<T::AssetId>,
        ValueQuery,
    >;

    // TODO: Combine the following maps into one using a custom struct
    /// map asset id to admin account
    #[pallet::storage]
    #[pallet::getter(fn asset_class_ownership)]
    pub(super) type AssetClassOwnership<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        Vec<T::AssetId>,
        ValueQuery,
    >;

    // map asset id to (cid, dataspaces)
    #[pallet::storage]
    #[pallet::getter(fn metadata)]
    pub(super) type Metadata<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AssetId,
        AssetMetadata,
    >;

    /// Store the map associating a node with the assets to which they have access
    ///
    /// asset_owner_accountid -> CID -> asset_class_owner_accountid
    /// 
    #[pallet::storage]
    #[pallet::getter(fn asset_access)]
    pub(super) type AssetAccess<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        Vec<T::AssetId>,
        ValueQuery,
    >;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
        /// A request to add bytes was queued
        CreatedIngestionRequest(T::AssetId),
        /// A request to retrieve bytes was queued
        QueuedDataToCat(T::AccountId),
        /// A new asset class was created (add bytes command processed)
        AssetClassCreated(T::AssetId),
        /// A new asset was created (tickets minted)
        AssetCreated(T::AssetId),
        /// An asset was burned succesfully
        AssetBurned(T::AssetId),
        /// A node has published ipfs identity results on chain
        PublishedIdentity(T::AccountId),
        /// data has been queued to be pinned by a storage node
        QueuedDataToPin,
	}

	#[pallet::error]
	pub enum Error<T> {
        /// could not build the ipfs request
		CantCreateRequest,
        /// the request to IPFS timed out
        RequestTimeout,
        /// the request to IPFS failed
        RequestFailed,
        /// The tx could not be signed
        OffchainSignedTxError,
        /// you cannot sign a tx
        NoLocalAcctForSigning,
        /// could not create a new asset
        CantCreateAssetClass,
        /// could not mint a new asset
        CantMintAssets,
        /// there is no asset associated with the specified cid
        NoSuchOwnedContent,
        /// the specified asset class does not exist
        NoSuchAssetClass,
        /// the account does not have a sufficient balance
        InsufficientBalance,
        /// the asset id is unknown or you do not have access to it
        InvalidAssetId,
        DataSpaceNotAccessible,
	}

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
         fn on_initialize(block_number: T::BlockNumber) -> Weight {
            // needs to be synchronized with offchain_worker actitivies
            if block_number % 2u32.into() == 1u32.into() {
                // <IngestionQueue<T>>::kill();
                <EjectionQueue<T>>::kill();
            }

            0
        }
    }

	#[pallet::call]
	impl<T: Config> Pallet<T> {

        /// submits an on-chain request to fetch data and add it to iris 
        /// 
        /// * `addr`: the multiaddress where the data exists
        ///       example: /ip4/192.168.1.170/tcp/4001/p2p/12D3KooWMvyvKxYcy9mjbFbXcogFSCvENzQ62ogRxHKZaksFCkAp
        /// * `cid`: the cid to fetch from the multiaddress
        ///       example: QmPZv7P8nQUSh2CpqTvUeYemFyjvMjgWEs8H1Tm8b3zAm9
        /// * `dataspace_id`: The asset id of the dataspace to associate the newly created asset class with
        /// * `id`: (temp) the unique id of the asset class -> should be generated instead
        /// * `balance`: the balance the owner is willing to use to back the asset class which will be created
        ///
        #[pallet::weight(100)]
        pub fn create(
            origin: OriginFor<T>,
            admin: <T::Lookup as StaticLookup>::Source,
            #[pallet::compact] dataspace_id: T::AssetId,
            #[pallet::compact] asset_id: T::AssetId,
            occ_id: Vec<u8>,
            #[pallet::compact] estimated_size_gb: u128,
            #[pallet::compact] balance: T::Balance,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            // check that the caller has access to the dataspace
            let balance = <pallet_assets::Pallet<T>>::balance(dataspace_id.clone(), who.clone());
            let balance_primitive = TryInto::<u128>::try_into(balance).ok();
            ensure!(balance_primitive != Some(0), Error::<T>::DataSpaceNotAccessible);
            // push a new command to the ingestion queue
            <IngestionQueue<T>>::mutate(|q| {
                q.push(
                    IngestionCommand {
                        owner: who.clone(),
                        estimated_size_gb,
                        balance,
                    }
                );
            });
            Self::deposit_event(Event::CreatedIngestionRequest(asset_id.clone()));
			Ok(())
        }

        /// Only callable by the owner of the asset class 
        /// mint a static number of assets (tickets) for some asset class
        ///
        /// * origin: should be the owner of the asset class
        /// * beneficiary: the address to which the newly minted assets are assigned
        /// * cid: a cid owned by the origin, for which an asset class exists
        /// * amount: the number of tickets to mint
        ///
        #[pallet::weight(100)]
        pub fn mint(
            origin: OriginFor<T>,
            beneficiary: <T::Lookup as StaticLookup>::Source,
            #[pallet::compact] asset_id: T::AssetId,
            #[pallet::compact] amount: T::Balance,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;

            let new_origin = system::RawOrigin::Signed(who.clone()).into();
            let beneficiary_accountid = T::Lookup::lookup(beneficiary.clone())?;
            <pallet_assets::Pallet<T>>::mint(
                new_origin, 
                asset_id.clone(), 
                beneficiary.clone(), 
                amount
            )?;
            
            <AssetAccess<T>>::mutate(beneficiary_accountid.clone(), |ids| { ids.push(asset_id.clone()) });
        
            Self::deposit_event(Event::AssetCreated(asset_id.clone()));
            Ok(())
        }

        /// transfer an amount of owned assets to another address
        /// 
        /// * `target`: The target node to receive the assets
        /// * `asset_id`: The asset id of the asset to be transferred
        /// * `amount`: The amount of the asset to transfer
        /// 
        #[pallet::weight(100)]
        pub fn transfer_asset(
            origin: OriginFor<T>,
            target: <T::Lookup as StaticLookup>::Source,
            #[pallet::compact] asset_id: T::AssetId,
            #[pallet::compact] amount: T::Balance,
        ) -> DispatchResult {
            let current_owner = ensure_signed(origin)?;

            let new_origin = system::RawOrigin::Signed(current_owner.clone()).into();
            <pallet_assets::Pallet<T>>::transfer(
                new_origin,
                asset_id.clone(),
                target.clone(),
                amount.clone(),
            )?;
            
            let target_account = T::Lookup::lookup(target)?;
            <AssetAccess<T>>::mutate(target_account.clone(), |ids| { ids.push(asset_id.clone()) });

            Ok(())
        }

        /// Burns the amount of assets
        /// 
        /// * `target`: the target account to burn assets from
        /// * `asset_id`: The asset id to burn
        /// * `amount`: The amount of assets to burn
        /// 
        #[pallet::weight(100)]
        pub fn burn(
            origin: OriginFor<T>,
            target: <T::Lookup as StaticLookup>::Source,
            #[pallet::compact] asset_id: T::AssetId,
            #[pallet::compact] amount: T::Balance,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let new_origin = system::RawOrigin::Signed(who.clone()).into();
            <pallet_assets::Pallet<T>>::burn(
                new_origin,
                asset_id.clone(),
                target,
                amount.clone(),
            )?;

            Self::deposit_event(Event::AssetBurned(asset_id.clone()));

            Ok(())
        }
    
        /// Create a new asset class on behalf of an admin node
        /// and submit a request to associate it with the specified dataspace
        /// 
        /// TODO: this is obviously insecure at the moment, as it is callable by
        /// any node. We will resolve this issue at a later date (once we have zk snarks)
        /// Technically, this function allows anyone to freely create 
        /// a new asset on someone's behalf
        ///
        /// * `admin`: The admin account
        /// * `cid`: The cid generated by the OCW
        /// * `dataspace_id`: The dataspace that the newly created asset class should be 
        ///                   associated with
        /// * `id`: The AssetId (passed through from the create_storage_asset call)
        /// * `balance`: The balance (passed through from the create_storage_asset call)
        ///
        #[pallet::weight(100)]
        pub fn submit_ipfs_add_results(
            origin: OriginFor<T>,
            admin: <T::Lookup as StaticLookup>::Source,
            cid: Vec<u8>,
            occ_id: Vec<u8>,
            dataspace_id: T::AssetId,
            #[pallet::compact] id: T::AssetId,
            #[pallet::compact] balance: T::Balance,
        ) -> DispatchResult {
            ensure_signed(origin)?;
            let which_admin = T::Lookup::lookup(admin.clone())?;
            let new_origin = system::RawOrigin::Signed(which_admin.clone()).into();

            <pallet_assets::Pallet<T>>::create(new_origin, id.clone(), admin.clone(), balance)
                .map_err(|_| Error::<T>::CantCreateAssetClass)?;

            let mut pending_dataspace_vec = Vec::new();
            pending_dataspace_vec.push(dataspace_id.clone());
            // insert into metadata for the asset class for the first time
            <Metadata<T>>::insert(id.clone(), AssetMetadata {
                cid: cid.clone(),
                occ_id: occ_id.clone(),
            });
            // TOOD: This should be its own queue
            // dispatch update dataspace metadata command
            <EjectionQueue<T>>::mutate(
                |queue| queue.push(DataCommand::AddToDataSpace( 
                    id.clone(),
                    dataspace_id.clone(),
                )));

            <AssetClassOwnership<T>>::mutate(which_admin, |ids| { ids.push(id) });
            <AssetIds<T>>::mutate(|ids| ids.push(id.clone()));
            
            Self::deposit_event(Event::AssetClassCreated(id.clone()));
            
            Ok(())
        }
	}
}

impl<T: Config> Pallet<T> {

}

/// a trait to provide the ingestion queue to other modules
pub trait QueueProvider<AccountId, Balance> {
    fn ingestion_queue() -> Vec<IngestionCommand<AccountId, Balance>>;
}

impl<T: Config> QueueProvider<T::AccountId, T::Balance> for Pallet<T> {
    fn ingestion_queue() -> Vec<IngestionCommand<T::AccountId, T::Balance>> {
        IngestionQueue::<T>::get()
    }
}