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

//! # Data Assets Pallet
//!
//! ## Overview
//!
//! ### Goals
//! This module provides functionality for data encryption and data asset class creation capabilities.
//! 
//! ### Dispatchable Functions 
//!
//! #### Permissionless functions
//! * create_storage_asset
//!
//! #### Permissioned Functions
//! * mint_tickets
//!
//! 
//! 

use scale_info::TypeInfo;
use codec::{Encode, Decode};
use frame_support::{
    pallet_prelude::*,
    traits::{Currency, LockableCurrency},
};
use frame_system::{
    self as system, 
    ensure_signed, 
    pallet_prelude::*,
    offchain::{
        AppCrypto, CreateSignedTransaction, SendUnsignedTransaction, SignedPayload, SubmitTransaction,
    },
};

use sp_runtime::{
    KeyTypeId,
    RuntimeDebug,
    traits::{
        Convert,
        StaticLookup,
        Verify,
    },
    transaction_validity::{
        InvalidTransaction, 
        TransactionValidity, 
        ValidTransaction
    },
};
use sp_std::{
    prelude::*,
};

use scale_info::prelude::string::ToString;
use sp_runtime::offchain::storage::StorageValueRef;
use generic_array::{
    GenericArray,
    typenum::UTerm,
};

use sp_core::{
    Bytes,
    sr25519::{Public, Signature},
};

use umbral_pre::*;
use rand_chacha::{
    ChaCha20Rng,
    rand_core::SeedableRng,
};
use crypto_box::{
    aead::{ AeadCore, Aead },
	SalsaBox, PublicKey as BoxPublicKey, SecretKey as BoxSecretKey, Nonce,
};

use core::convert::TryInto;
use pallet_vesting::VestingInfo;
use iris_primitives::{IngestionCommand, EncryptionResult, EncryptedFragment};

/// struct to store metadata of an asset class
#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub struct AssetMetadata {
    /// the cid of some data
    pub cid: Vec<u8>,
    /// the public key associated with the encryption artifacts (capsule and fragments)
    pub public_key: Vec<u8>,
}

// TODO: These structs are really getting out of hand

#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub struct SecretStuff {
    pub data_capsule: Vec<u8>,
    pub sk_capsule: Vec<u8>,
    pub sk_ciphertext: Vec<u8>,
}

#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub struct EncryptedData {
    pub capsule: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

#[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
pub struct CapsuleRecoveryRequest<AccountId> {
    pub caller: AccountId,
    pub public_key: Vec<u8>,
}

// #[derive(Encode, Decode, RuntimeDebug, PartialEq, TypeInfo)]
// pub struct ReencryptionRequest<AccountId> {
//     pub caller: AccountId,
//     pub data_public_key: Vec<u8>,
//     pub caller_public_key: Vec<u8>,
// }

pub const KEY_TYPE: KeyTypeId = KeyTypeId(*b"iris");

/// Based on the above `KeyTypeId` we need to generate a pallet-specific crypto type wrappers.
/// We can use from supported crypto kinds (`sr25519`, `ed25519` and `ecdsa`) and augment
/// the types with this pallet-specific identifier.
pub mod crypto {
	use super::KEY_TYPE;
	use sp_core::sr25519::Signature as Sr25519Signature;
	use sp_runtime::{
		app_crypto::{app_crypto, sr25519},
		traits::Verify,
		MultiSignature, MultiSigner,
	};
	app_crypto!(sr25519, KEY_TYPE);

	pub struct TestAuthId;

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

type BalanceOf<T> =
	<<T as pallet_vesting::Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

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
	use frame_support::dispatch::DispatchResult;
	use sp_std::{
        str,
    };

	#[pallet::config]
	pub trait Config: frame_system::Config + 
        CreateSignedTransaction<Call<Self>> + 
        pallet_assets::Config + 
        pallet_vesting::Config +
        pallet_authorities::Config
    {
        /// The overarching event type
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
        /// the overarching call type
	    type Call: From<Call<Self>>;
        /// The currency trait.
		type Currency: LockableCurrency<Self::AccountId>;
        /// the authority id used for sending signed txs
        type AuthorityId: AppCrypto<Self::Public, Self::Signature>;
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
    #[pallet::without_storage_info]
	pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn ingestion_commands)]
    pub(super) type IngestionCommands<T: Config> = StorageMap<
        _, 
        Blake2_128Concat,
        T::AccountId, 
        Vec<IngestionCommand<T::AccountId, T::Balance>>, 
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn next_asset_id)]
    pub(super) type NextAssetId<T: Config> = StorageValue<_, T::AssetId, ValueQuery>;

    // map asset id to (cid, dataspaces)
    #[pallet::storage]
    #[pallet::getter(fn metadata)]
    pub(super) type Metadata<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AssetId,
        AssetMetadata,
        OptionQuery
    >;

    #[pallet::storage]
    pub type Delay<T: Config> = StorageValue<_, u32, ValueQuery>;

    /// The staging map maps account ids to the public key that 
    /// corresponds to data they've encrypted but have not yet ingested
    /// We make the assumption that a node is only allowed to stage
    /// a single encrypted dataset at once (for now, open to changing that)
    #[pallet::storage]
    pub type IngestionStaging<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        Vec<u8>,
        OptionQuery,
    >;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
        /// A request to add bytes was queued
        CreatedIngestionRequest,
	}

	#[pallet::error]
	pub enum Error<T> {
        /// could not create a new asset
        CantCreateAssetClass,
	}


    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        pub initial_asset_id: T::AssetId,
        pub delay: u32,
    }

    #[cfg(feature = "std")]
    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            let base_asset_id: u32 = 2u32;
            let asset_id = TryInto::<T::AssetId>::try_into(base_asset_id).ok().unwrap();
            GenesisConfig {
                initial_asset_id: asset_id,
                delay: 10,
            }
        }
    }

    #[pallet::genesis_build]
    impl<T: Config> GenesisBuild<T> for GenesisConfig<T> {
        fn build(&self) {
            NextAssetId::<T>::put(self.initial_asset_id);
            Delay::<T>::put(self.delay);
        }
    }

	#[pallet::call]
	impl<T: Config> Pallet<T> {

        /// submits an on-chain request to fetch data and add it to iris 
        /// 
        /// * `gateway`: The gateway node that should verify the data.
        /// * `multiaddress`: the multiaddress where the data exists
        ///       example: /ip4/192.168.1.170/tcp/4001/p2p/12D3KooWMvyvKxYcy9mjbFbXcogFSCvENzQ62ogRxHKZaksFCkAp
        /// * `cid`: the cid to fetch from the multiaddress
        ///       example: QmPZv7P8nQUSh2CpqTvUeYemFyjvMjgWEs8H1Tm8b3zAm9
        /// * `dataspace_id`: The asset id of the dataspace to associate the newly created asset class with
        /// * `id`: (temp) the unique id of the asset class -> should be generated instead
        /// * `balance`: the balance the owner is willing to use to back the asset class which will be created
        ///
        #[pallet::weight(100)]
        pub fn create_request(
            origin: OriginFor<T>,
            gateway: <T::Lookup as StaticLookup>::Source,
            gateway_reserve: BalanceOf<T>,
            cid: Vec<u8>,
            multiaddress: Vec<u8>,
            estimated_size_gb: u128,
            #[pallet::compact] min_asset_balance: T::Balance,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let g = T::Lookup::lookup(gateway.clone())?; 
            let mut commands = IngestionCommands::<T>::get(g.clone());
            let cmd = IngestionCommand {
                owner: who.clone(),
                cid: cid,
                multiaddress: multiaddress,
                estimated_size_gb: estimated_size_gb,
                balance: min_asset_balance,
            };
            commands.push(cmd.clone());
            IngestionCommands::<T>::insert(g.clone(), commands);

            let current_block_number = <frame_system::Pallet<T>>::block_number();
            let target_block = current_block_number + Delay::<T>::get().into();
            let new_origin = system::RawOrigin::Signed(who.clone()).into();
            // vest currency
            <pallet_vesting::Pallet<T>>::vested_transfer(
                new_origin, gateway, 
                VestingInfo::new(gateway_reserve, gateway_reserve, target_block),
            )?;
            Self::deposit_event(Event::CreatedIngestionRequest);
			Ok(())
        }

        /// increase the balance vested in the request 
        /// sent to a gateway
        #[pallet::weight(100)]
        pub fn bump_request(
            origin: OriginFor<T>,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            // TODO
            Ok(())
        }

        /// if a request has not been processed, 'unvest' total balance
        #[pallet::weight(100)]
        pub fn kill_request(
            origin: OriginFor<T>,
        ) -> DispatchResult {
            // TODO
            Ok(())
        }


    }
}

impl<T: Config> Pallet<T> {
    // a super simple asset id generator and mutator
    // needs to be modified so we don't have duplicate asset id
    // TODO: this approach will fail, undoubtedly
    fn get_and_increment_asset_id() -> T::AssetId {
        let next = NextAssetId::<T>::get();
        let primitive = TryInto::<u32>::try_into(next).ok().unwrap();
        let new_id = primitive + 1u32;
        let new_next_asset_id = TryInto::<T::AssetId>::try_into(new_id).ok().unwrap();
        NextAssetId::<T>::mutate(|id| *id = new_next_asset_id);
        next
    }

    /// validates if an unsigned tx is valid
    /// for now, all are valid
    fn validate_transaction_parameters() -> TransactionValidity {
		ValidTransaction::with_tag_prefix("iris")
			.longevity(5)
			.propagate(true)
			.build()
	}

    // /// TODO: should it be signed or unsigned tx? probably signed right?
    // /// checkout: client\network\src\config.rs for sk generation/storage + write to file
    // /// Recover signing acct and use it to encrypt the data and submit unsigned tx
    // pub fn encrypt(
    //     plaintext: Bytes,
    //     signature: Bytes,
    //     signer: Bytes,
    //     message: Bytes,
    //     shares: usize,
    //     threshold: usize,
    //     proxy: Bytes,
    // ) -> Option<Bytes> {

    //     let proxy_acct_bytes: [u8;32] = proxy.to_vec().try_into().unwrap();
    //     // let proxy_acct_pubkey = Public::from_raw(proxy_acct_bytes);
    //     let proxy_acct_id: T::AccountId = T::AccountId::decode(&mut &proxy_acct_bytes[..]).unwrap();

    //     let acct_bytes: [u8;32] = signer.to_vec().try_into().unwrap();
    //     let acct_pubkey = Public::from_raw(acct_bytes);
    //     let sig: Signature = Signature::from_slice(signature.to_vec().as_ref()).unwrap();
    //     let msg: Vec<u8> = message.to_vec();

    //     let acct_id: T::AccountId = T::AccountId::decode(&mut &acct_bytes[..]).unwrap();

    //     if sig.verify(msg.as_slice(), &acct_pubkey) {
    //         let proxy_pk_vec = pallet_authorities::Pallet::<T>::x25519_public_keys(proxy_acct_id.clone());
    //         let proxy_pk_slice = iris_primitives::slice_to_array_32(&proxy_pk_vec).unwrap();
    //         let proxy_pk = BoxPublicKey::from(*proxy_pk_slice);

    //         let plaintext_as_slice: &[u8] = &plaintext.to_vec();

    //         // match T::OffchainKeyManager::encrypt(plaintext, shares, threshold, proxy_pk) {
    //         //     Some(ciphertext) => { },
    //         //     None => { }
    //         // }

    //         match iris_primitives::encrypt_phase_1(plaintext_as_slice, shares, threshold, proxy_pk) {
    //             // capsule, ciphertext, public key, encrypted secret key
    //             Ok(result) => {
    //                 let data_capsule_vec: Vec<u8> = result.0.to_array().as_slice().to_vec();
    //                 let pk: Vec<u8> = result.2.to_array().as_slice().to_vec();

    //                 let call = Call::submit_encryption_artifacts { 
    //                     owner: acct_id,
    //                     data_capsule: data_capsule_vec,
    //                     public_key: pk.clone(),
    //                     proxy: proxy_acct_id.clone(),
    //                     sk_encryption_info: result.3.clone(),
    //                 };
        
    //                 SubmitTransaction::<T, Call<T>>::submit_unsigned_transaction(call.into())
    //                     .map_err(|()| "Unable to submit unsigned transaction.");
            
    //                 Some(Bytes::from(result.1.to_vec()))
    //             },
    //             Err(e) => {
    //                 Some(Bytes::from("".as_bytes().to_vec()))
    //             }
    //         };
    //     }

    //     None 
    // }

    /// Attempt to decrypt the ciphertext
    /// 
    /// * `signature`: 
    /// * `signer`:
    /// * `message`:
    /// * `ciphertext`:
    /// * `asset_id`: 
    /// * `secret_key`: This is the secret key that corresponds to the public key passed as an argument
    ///                 when requesting authorization through the rule executor. This is used to decrypt
    ///                 the encrypted capsule fragments
    /// 
    pub fn decrypt(
        signature: Bytes,
        signer: Bytes,
        message: Bytes,
        ciphertext: Bytes,
        asset_id: u32,
        secret_key_bytes: Bytes,
    ) -> Option<Bytes> {
        // let acct_bytes: [u8;32] = signer.to_vec().try_into().unwrap();
        // let acct_pubkey = Public::from_raw(acct_bytes);
        // let sig: Signature = Signature::from_slice(signature.to_vec().as_ref()).unwrap();
        // let msg: Vec<u8> = message.to_vec();

        // let acct_id: T::AccountId = T::AccountId::decode(&mut &acct_bytes[..]).unwrap();

        // // WARNING: this won't compile as is
        // if sig.verify(msg.as_slice(), &acct_pubkey) {
        //     // recover encrypted capsules
        //     let asset_id_converted = TryInto::<T::AssetId>::try_into(asset_id).ok().unwrap();
        //     let verified_capsule_fragments = VerifiedCapsuleFrags::<T>::get(acct_id, asset_id_converted.clone());
        //     // decrypt capsules
        //     let sk_vec = secret_key_bytes.clone().to_vec();
        //     let sk_proto_slice = sk_vec.as_slice();
        //     let secret_key_slice = iris_primitives::slice_to_array_32(sk_proto_slice).unwrap();
        //     let secret_key = BoxSecretKey::from(*secret_key_slice);
        //     let capsule_fragments = Self::decrypt_capsule_fragments(
        //         verified_capsule_fragments, secret_key,
                
        //     );
        //     let data_public_key = Metadata::<T>::get(asset_id_converted.clone()).unwrap().public_key;
        //     // fetch the capsule
        //     let capsule = Capsules::<T>::get(data_public_key).unwrap();
        //     // now verify each fragment
        //     // let verified_fragments = capsule_fragments.iter().map(|frag| {
        //     //     frag.verify(&capsule, &verifying_pk, &original_validator_pk, &my_pk).unwrap()
        //     // });
        //     // let plaintext_sk = umbral_pre::decrypt_reencrypted(

        //     // ).unwrap();
        //     // use capsule fragments to re-encrypt secret key for yourself
        //     // verify each one 
        //     // let verified_fragments = capsule_fragments.iter().map(|cfrag| cfrag.verify(
        //     //     &capsule, & verifying_pk, &alice_pk, &bob_pk,
        //     // ));
        //     // // use reencrypted key to decrypt the ciphertext
        //     // let sk_plaintext_bob = umbral_pre::decrypt_reencrypted(
        //     //     &my_sk, &alice_pk, &capsule, verified_fragments, &sk_ciphertext,
        //     // ).unwrap().to_vec();

        //     // let recovered_sk = SecretKey::from(sk_plaintext_bob);
        //     // let plaintext = umbral_pre::decrypt_original(&recovered_sk, &data_capsule, &ciphertext).unwrap();

        //     // return ciphertext
        //     // encryption::decrypt(acct_id, ciphertext, asset_id)
        // }
        Some(Bytes::from(Vec::new()))

    }
    // an interesting thought: any way to make an implementation of from/into that lets us encrypt/decrypt?
    /// Attempt to decrypt an encrypted capsule fragment using the secret key
    /// 
    /// * `encrypted_cfrags`: A Vec of EncryptedFragment
    /// * `secret_key`: A secret that should be able to decrypt each encrypted capsule fragment
    /// 
    pub fn decrypt_capsule_fragments(
        encrypted_cfrags: Vec<EncryptedFragment>,
        secret_key: BoxSecretKey,
    ) -> Vec<VerifiedCapsuleFrag> {
        encrypted_cfrags.iter().map(|frag| {
            let pubkey_slice_32 = iris_primitives::slice_to_array_32(frag.public_key.as_slice()).unwrap();
            let ephemeral_pk = BoxPublicKey::from(*pubkey_slice_32);
            // use the ephemeral public key used to encrypt the cfrag
            let ephemeral_box = SalsaBox::new(&ephemeral_pk, &secret_key);
            let gen_array = generic_array::GenericArray::clone_from_slice(frag.nonce.as_slice());
            let decrypted_frag_vec = ephemeral_box.decrypt(&gen_array, &frag.ciphertext[..]).unwrap().to_vec();
            // hmm... I may have to UNVERIFY the frags first? otherwise this should work, though 
            // I'm not really sure how safe this is
            VerifiedCapsuleFrag::from_verified_bytes(decrypted_frag_vec).unwrap()
        }).collect::<Vec<_>>()
    }
}

pub trait MetadataProvider<AssetId> {
    fn get(asset_id: AssetId) -> Option<AssetMetadata>;
}

impl<T: Config> MetadataProvider<T::AssetId> for Pallet<T> {
    fn get(asset_id: T::AssetId) -> Option<AssetMetadata> {
        Metadata::<T>::get(asset_id)
    }
}

// Implementation of Convert trait for mapping ValidatorId with AccountId.
pub struct ValidatorOf<T>(sp_std::marker::PhantomData<T>);

impl<T: Config> Convert<T::ValidatorId, Option<T::ValidatorId>> for ValidatorOf<T> {
	fn convert(account: T::ValidatorId) -> Option<T::ValidatorId> {
		Some(account)
	}
}

/// a trait to provide the ingestion queue to other modules
pub trait QueueManager<AccountId, Balance> {

    fn add_ingestion_staging(owner: AccountId, public_key: Vec<u8>);
    fn ingestion_requests(gateway: AccountId) -> Vec<IngestionCommand<AccountId, Balance>>;

    // fn capsule_recovery_requests();
    // fn add_capsule_recovery_request();

}

impl<T: Config> QueueManager<T::AccountId, T::Balance> for Pallet<T> {

    fn add_ingestion_staging(owner: T::AccountId, public_key: Vec<u8>) {
        IngestionStaging::<T>::insert(owner.clone(), public_key.clone());
    }

    fn ingestion_requests(gateway: T::AccountId) -> Vec<IngestionCommand<T::AccountId, T::Balance>> {
        IngestionCommands::<T>::get(gateway)
    }
}

/// The result handler allows other modules to submit "execution"
/// of commands added to the queue
/// honestly at this point... it almost seems like it'd make more sense to bake all this
/// into the consensus mechanism itself, i.e. babe/aura
/// basically I'm implementing a parallel consensus mechanism to determine who gets to proxy requests
pub trait ResultsHandler<T: frame_system::Config, AccountId, Balance> {
    fn create_asset_class(
        origin: OriginFor<T>,
        cmd: IngestionCommand<AccountId, Balance>
    ) -> DispatchResult;
}

impl<T: Config> ResultsHandler<T, T::AccountId, T::Balance> for Pallet<T> {
    // this is just an extrinsic...
    fn create_asset_class(
        origin: OriginFor<T>,
        cmd: IngestionCommand<T::AccountId, T::Balance>,
    ) -> DispatchResult {
        let who = ensure_signed(origin)?;

        // verify that capsule exists (i.e. data is 'decryptable')
        match IngestionStaging::<T>::get(cmd.owner.clone()) {
            Some(pubkey) => {
                let asset_id = Self::get_and_increment_asset_id();
                let admin = T::Lookup::unlookup(cmd.clone().owner);
                let new_origin = system::RawOrigin::Signed(who.clone()).into();
                <pallet_assets::Pallet<T>>::create(new_origin, asset_id.clone(), admin.clone(), cmd.balance.clone())
                    .map_err(|e| {
                        log::info!("Failed to create asset class due to error: {:?}", e);
                        return Error::<T>::CantCreateAssetClass;
                    })?;
                <Metadata<T>>::insert(asset_id.clone(), AssetMetadata {
                    cid: cmd.cid.clone(),
                    public_key: pubkey,
                });
                IngestionStaging::<T>::remove(cmd.clone().owner);
                // remove from ingestion commands, this must be done before the 'now + delay' number of blocks passes
                // for now... let's just assume there is not time limit and test it out
                let mut cmds = IngestionCommands::<T>::get(who.clone());
                let cmd_idx = cmds.iter().position(|c| *c == cmd.clone()).unwrap();
                cmds.remove(cmd_idx);
                IngestionCommands::<T>::insert(who.clone(), cmds); // needed?
                // emit event?
                Ok(())
            },
            None => {
                // Error instead?
                Ok(())
            }
        }
    }
}