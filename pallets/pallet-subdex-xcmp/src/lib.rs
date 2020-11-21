// Copyright 2020 Parity Technologies (UK) Ltd.
// This file is part of Cumulus.

// Cumulus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Cumulus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Cumulus.  If not, see <http://www.gnu.org/licenses/>.

//! Example Pallet that shows how to send upward messages and how to receive
//! downward messages.
#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::{decl_error, decl_event, decl_module, decl_storage, ensure, traits::Currency};
use frame_system::ensure_signed;

use codec::{Codec, Decode, Encode};
use cumulus_primitives::{
    relay_chain::DownwardMessage,
    xcmp::{XCMPMessageHandler, XCMPMessageSender},
    DownwardMessageHandler, ParaId, UpwardMessageOrigin, UpwardMessageSender,
};
use cumulus_upward_message::BalancesMessage;
pub use pallet_subdex::Asset;
pub use sp_arithmetic::traits::{One, Zero};

#[derive(Encode, Decode)]
pub enum XCMPMessage<XAccountId, XBalance, XAssetIdOf> {
    /// Transfer tokens to the given account from the Parachain account.
    /// When XAssetIdOf is None, treat message as main currency transfer.
    TransferToken(XAccountId, XBalance, Option<XAssetIdOf>),
}

pub type BalanceOf<T> = <<T as pallet_subdex::Trait>::Currency as Currency<
    <T as frame_system::Trait>::AccountId,
>>::Balance;

pub type AssetIdOf<T> = <T as pallet_subdex::Trait>::AssetId;

/// Configuration trait of this pallet.
pub trait Trait: frame_system::Trait + pallet_subdex::Trait {
    /// Event type used by the runtime.
    type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;

    /// The sender of upward messages.
    type UpwardMessageSender: UpwardMessageSender<Self::UpwardMessage>;

    /// The upward message type used by the Parachain runtime.
    type UpwardMessage: Codec + BalancesMessage<Self::AccountId, BalanceOf<Self>>;

    /// The sender of XCMP messages.
    type XCMPMessageSender: XCMPMessageSender<
        XCMPMessage<Self::AccountId, BalanceOf<Self>, AssetIdOf<Self>>,
    >;
}

// This pallet's storage items.
decl_storage! {
    trait Store for Module<T: Trait> as ParachainUpgrade {

        // Maps parachain asset id to our internal respresentation
        pub AssetIdByParaAssetId get(fn asset_id_by_para_asset_id):
            double_map hasher(blake2_128_concat) ParaId, hasher(blake2_128_concat) Option<AssetIdOf<T>> => AssetIdOf<T>;

        // Next dex parachain asset id
        pub NextAssetId get(fn next_asset_id) config(): AssetIdOf<T>;
    }
}

decl_event! {
    pub enum Event<T> where
        AccountId = <T as frame_system::Trait>::AccountId,
        Balance = BalanceOf<T>,
        // None if main currency
        ParaChainAssetId = Option<AssetIdOf<T>>,
        // Our internal para asset id representation
        DexAssetId = AssetIdOf<T>

    {
        /// Transferred main currency amount to the account on the relay chain.
        TransferredTokensToRelayChain(AccountId, Balance),

        /// Transferred main currency amount  to the account on request from the relay chain.
        TransferredTokensFromRelayChain(AccountId, Balance),

        /// Transferred custom asset to the account from the given parachain account.
        DepositAssetViaXCMP(ParaId, ParaChainAssetId, AccountId, DexAssetId, Balance),

        /// Transferred custom asset to the account from the given parachain account.
        WithdrawAssetViaXCMP(ParaId, ParaChainAssetId, AccountId, DexAssetId, Balance),
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin, system = frame_system {

        fn deposit_event() = default;

        /// Transfer `amount` of main currency on the relay chain from the Parachain account to
        /// the given `dest` account.
        #[weight = 10]
        fn transfer_balance_to_relay_chain(origin, dest: T::AccountId, amount: BalanceOf<T>) {
            let sender = ensure_signed(origin)?;

            Self::ensure_non_zero_balance(amount)?;

            <pallet_subdex::Module<T>>::ensure_sufficient_balance(&sender, Asset::MainNetworkCurrency, amount)?;

            //
            // == MUTATION SAFE ==
            //

            <pallet_subdex::Module<T>>::slash_asset(&sender, Asset::MainNetworkCurrency, amount);


            let msg = <T as Trait>::UpwardMessage::transfer(dest.clone(), amount);
            <T as Trait>::UpwardMessageSender::send_upward_message(&msg, UpwardMessageOrigin::Signed)
                .expect("Should not fail; qed");

            Self::deposit_event(Event::<T>::TransferredTokensToRelayChain(dest, amount));
        }

        // Transfer an `amount` of another parachain asset.
        #[weight = 10]
        fn transfer_asset_balance_to_parachain_chain(
            origin,
            para_id: u32,
            dest: T::AccountId,
            para_asset_id: Option<AssetIdOf<T>>,
            amount: BalanceOf<T>,
        ) {

            //TODO we don't make sure that the parachain has some tokens on the other parachain.
            let who = ensure_signed(origin)?;

            Self::ensure_non_zero_balance(amount)?;

            let para_id: ParaId = para_id.into();

            // Retreive our internal para asset id representation
            let asset_id = Self::ensure_asset_id_exists(para_id, para_asset_id)?;

            <pallet_subdex::Module<T>>::ensure_sufficient_balance(&who, Asset::ParachainAsset(asset_id), amount)?;

            //
            // == MUTATION SAFE ==
            //

            <pallet_subdex::Module<T>>::slash_asset(&who, Asset::ParachainAsset(asset_id), amount);

            T::XCMPMessageSender::send_xcmp_message(
                para_id,
                &XCMPMessage::TransferToken(dest.clone(), amount, para_asset_id),
            ).expect("Should not fail; qed");

            Self::deposit_event(Event::<T>::WithdrawAssetViaXCMP(para_id, para_asset_id, dest, asset_id, amount));
        }

    }
}

/// This is a hack to convert from one generic type to another where we are sure that both are the
/// same type/use the same encoding.
fn convert_hack<O: Decode>(input: &impl Encode) -> O {
    input.using_encoded(|e| Decode::decode(&mut &e[..]).expect("Must be compatible; qed"))
}

impl<T: Trait> DownwardMessageHandler for Module<T> {
    /// Transfer main network asset into dex parachain from the relay chain (natively supported via Currency trait)
    fn handle_downward_message(msg: &DownwardMessage) {
        if let DownwardMessage::TransferInto(dest, amount, _) = msg {
            let dest = convert_hack(&dest);
            let amount: BalanceOf<T> = convert_hack(amount);

            <pallet_subdex::Module<T>>::ensure_can_hold_balance(
                &dest,
                Asset::MainNetworkCurrency,
                amount,
            )
            .expect("Should not fail!");

            //
            // == MUTATION SAFE ==
            //

            <pallet_subdex::Module<T>>::mint_asset(&dest, Asset::MainNetworkCurrency, amount);

            Self::deposit_event(Event::<T>::TransferredTokensFromRelayChain(dest, amount));
        }
    }
}

impl<T: Trait> XCMPMessageHandler<XCMPMessage<T::AccountId, BalanceOf<T>, AssetIdOf<T>>>
    for Module<T>
{
    // Transfer main currency or custom asset from other parachain to our chain
    fn handle_xcmp_message(
        src: ParaId,
        msg: &XCMPMessage<T::AccountId, BalanceOf<T>, AssetIdOf<T>>,
    ) {
        let asset_id = match msg {
            XCMPMessage::TransferToken(dest, amount, para_asset_id)
                if <AssetIdByParaAssetId<T>>::contains_key(src, para_asset_id) =>
            {
                let asset_id = Self::asset_id_by_para_asset_id(src, para_asset_id);

                <pallet_subdex::Module<T>>::ensure_can_hold_balance(
                    &dest,
                    Asset::ParachainAsset(asset_id),
                    *amount,
                )
                .expect("Should not fail!");
                Some(asset_id)
            }
            _ => None,
        };

        //
        // == MUTATION SAFE ==
        //

        match msg {
            XCMPMessage::TransferToken(dest, amount, para_asset_id) => {
                if let Some(asset_id) = asset_id {
                    <pallet_subdex::Module<T>>::mint_asset(
                        &dest,
                        Asset::ParachainAsset(asset_id),
                        *amount,
                    );
                    Self::deposit_event(Event::<T>::DepositAssetViaXCMP(
                        src,
                        // para asset_id
                        *para_asset_id,
                        dest.clone(),
                        // internal asset id representation
                        asset_id,
                        *amount,
                    ));
                } else {
                    let next_asset_id = Self::next_asset_id();
                    <AssetIdByParaAssetId<T>>::insert(src, *para_asset_id, next_asset_id);

                    <pallet_subdex::Module<T>>::mint_asset(
                        &dest,
                        Asset::ParachainAsset(next_asset_id),
                        *amount,
                    );

                    <NextAssetId<T>>::mutate(|asset_id| *asset_id += AssetIdOf::<T>::one());

                    Self::deposit_event(Event::<T>::DepositAssetViaXCMP(
                        src,
                        // para asset_id
                        *para_asset_id,
                        dest.clone(),
                        // internal asset id representation
                        next_asset_id,
                        *amount,
                    ));
                }
            }
        }
    }
}

impl<T: Trait> Module<T> {
    pub fn ensure_asset_id_exists(
        para_id: ParaId,
        para_asset_id: Option<AssetIdOf<T>>,
    ) -> Result<AssetIdOf<T>, Error<T>> {
        ensure!(
            <AssetIdByParaAssetId<T>>::contains_key(para_id, para_asset_id),
            Error::<T>::AssetIdDoesNotExist
        );
        Ok(Self::asset_id_by_para_asset_id(para_id, para_asset_id))
    }

    pub fn ensure_non_zero_balance(amount: BalanceOf<T>) -> Result<(), Error<T>> {
        ensure!(
            amount > BalanceOf::<T>::zero(),
            Error::<T>::ZeroBalanceAmount
        );
        Ok(())
    }
}

decl_error! {
    pub enum Error for Module<T: Trait> {
        // Transferred amount should be greater than 0
        AmountShouldBeGreaterThanZero,
        // Given parachain asset id entry does not exist
        AssetIdDoesNotExist,
        ZeroBalanceAmount,
    }
}
