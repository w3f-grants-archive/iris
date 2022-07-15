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

pub use self::gen_client::Client as IrisClient;
use jsonrpc_core::{Error as RpcError, ErrorCode, Result};
use jsonrpc_derive::rpc;
pub use pallet_iris_rpc_runtime_api::IrisApi as IrisRuntimeApi;
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_core::{
	Bytes,
	sr25519::{Signature, Public}
};
use sp_runtime::{
	generic::BlockId,
	traits::{Block as BlockT},
};
use std::sync::Arc;
use codec::{Codec, Decode, Encode};
use sp_std::vec::Vec;

#[rpc(client, server)]
pub trait IrisApi<BlockHash> {

	#[rpc(name = "iris_addBytes")]
	fn retrieve_bytes(
		&self,
		byte_stream: Bytes,
		asset_id: u32,
		signature: Bytes,
		signer: Bytes,
		message: Bytes,
		at: Option<BlockHash>,
	) -> Result<Bytes>;

	#[rpc(name = "iris_retrieveBytes")]
	fn retrieve_bytes(
		&self,
		asset_id: u32,
		at: Option<BlockHash>,
	) -> Result<Bytes>;
}

/// A struct that implements IrisRpc
pub struct Iris<C, M> {
	client: Arc<C>,
	_marker: std::marker::PhantomData<M>,
}

impl<C, M> Iris<C, M> {
	/// create new 'Iris' instance with the given reference	to the client
	pub fn new(client: Arc<C>) -> Self {
		Self { client, _marker: Default::default() }
	}
}

pub enum Error {
	RuntimeError,
	DecodeError,
}

impl From<Error> for i64 {
	fn from(e: Error) -> i64 {
		match e {
			Error::RuntimeError => 1,
			Error::DecodeError => 2,
		}
	}
}

impl<C, Block> IrisApi<<Block as BlockT>::Hash>
	for Iris<C, Block>
where 
	Block: BlockT,
	C: Send + Sync + 'static,
	C: ProvideRuntimeApi<Block>,
	C: HeaderBackend<Block>,
	C::Api: IrisRuntimeApi<Block>,
{

	fn add_bytes(
		&self,
		byte_stream: Bytes,
		asset_id: u32,
		signature: Bytes,
		signer: Bytes,
		message: Bytes,
		at: Option<Block as BlockT>::Hash,
	) -> Result<Bytes> {
		let api = self.client.runtime_api();
		let at = BlockId::hash(at.unwrap_or_else(||
			self.client.info().best_hash
		));
		let runtime_api_result = api.retrieve_bytes(&at, asset_id);
		runtime_api_result.map_err(|e| RpcError{
			code: ErrorCode::ServerError(Error::DecodeError.into()),
			message: "unable to query runtime api".into(),
			data: Some(format!("{:?}", e).into()),
		})
	}

	fn retrieve_bytes(
		&self,
		asset_id: u32,
		at: Option<<Block as BlockT>::Hash>,
	) -> Result<Bytes> {
		let api = self.client.runtime_api();
		let at = BlockId::hash(at.unwrap_or_else(||
			self.client.info().best_hash
		));
		let runtime_api_result = api.retrieve_bytes(&at, asset_id);
		runtime_api_result.map_err(|e| RpcError{
			code: ErrorCode::ServerError(Error::DecodeError.into()),
			message: "unable to query runtime api".into(),
			data: Some(format!("{:?}", e).into()),
		})
	}
}