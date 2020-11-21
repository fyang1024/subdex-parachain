use super::*;

/// Structure, representing exchange pool
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
pub struct Exchange<T: Trait> {
    first_asset_pool: BalanceOf<T>,
    second_asset_pool: BalanceOf<T>,
    pub invariant: BalanceOf<T>,
    // total pool shares
    pub total_shares: BalanceOf<T>,
    // last timestamp, after pool update performed, needed for time_elapsed calculation
    pub last_timestamp: T::IMoment,
    // first_asset_pool / second_asset_pool * time_elapsed
    pub price1_cumulative_last: BalanceOf<T>,
    // second_asset_pool / first_asset_pool * time_elapsed
    pub price2_cumulative_last: BalanceOf<T>,
    // individual shares
    shares: BTreeMap<T::AccountId, BalanceOf<T>>,
}

impl<T: Trait> Default for Exchange<T> {
    fn default() -> Self {
        Self {
            first_asset_pool: BalanceOf::<T>::default(),
            second_asset_pool: BalanceOf::<T>::default(),
            invariant: BalanceOf::<T>::default(),
            total_shares: BalanceOf::<T>::default(),
            last_timestamp: <pallet_timestamp::Module<T>>::get().into(),
            price1_cumulative_last: BalanceOf::<T>::default(),
            price2_cumulative_last: BalanceOf::<T>::default(),
            shares: BTreeMap::new(),
        }
    }
}

#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
pub struct SwapDelta<T: Trait> {
    pub first_asset_pool: BalanceOf<T>,
    pub second_asset_pool: BalanceOf<T>,
    // Either first or second asset amount (depends on swap direction)
    pub amount: BalanceOf<T>,
}

impl<T: Trait> SwapDelta<T> {
    pub fn new(
        first_asset_pool: BalanceOf<T>,
        second_asset_pool: BalanceOf<T>,
        amount: BalanceOf<T>,
    ) -> Self {
        Self {
            first_asset_pool,
            second_asset_pool,
            amount,
        }
    }
}

impl<T: Trait> Exchange<T> {
    // Avoid casting to float
    fn sqrt(y: BalanceOf<T>) -> Result<BalanceOf<T>, Error<T>> {
        let z = if y > 3.into() {
            let mut z = y;
            let mut x = y
                .checked_div(&2.into())
                .map(|res| res.checked_add(&1.into()))
                .flatten()
                .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
            while x < z {
                z = x;
                x = y
                    .checked_div(&(x + x))
                    .map(|res| res.checked_div(&2.into()))
                    .flatten()
                    .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
            }
            z
        } else if y != BalanceOf::<T>::zero() {
            BalanceOf::<T>::one()
        } else {
            BalanceOf::<T>::zero()
        };
        Ok(z)
    }

    // Reconsider this approach after setting
    // first_asset & second_asset minimal amount restrictions

    // fn get_min_fee() -> BalanceOf<T> {
    //     match core::mem::size_of::<BalanceOf<T>>() {
    //         size if size <= 64 => 1.into(),
    //         // cosider 112 instead
    //         size if size > 64 && size < 128 => 10.into(),
    //         _ => (10 * 10 * 10).into(),
    //     }
    // }

    pub fn initialize_new(
        first_asset_amount: BalanceOf<T>,
        second_asset_amount: BalanceOf<T>,
        sender: T::AccountId,
    ) -> Result<(Self, BalanceOf<T>), Error<T>> {
        let mut shares_map = BTreeMap::new();
        // let min_fee = Self::get_min_fee();

        let initial_shares = Self::sqrt(first_asset_amount * second_asset_amount)?;
        // .checked_sub(&min_fee)
        // .ok_or(Error::<T>::UnderflowOccured)?;

        shares_map.insert(sender, initial_shares);
        let exchange = Self {
            first_asset_pool: first_asset_amount,
            second_asset_pool: second_asset_amount,
            invariant: first_asset_amount
                .checked_mul(&second_asset_amount)
                .ok_or(Error::<T>::UnderflowOrOverflowOccured)?,
            total_shares: initial_shares,
            shares: shares_map,
            last_timestamp: <pallet_timestamp::Module<T>>::get().into(),
            price1_cumulative_last: BalanceOf::<T>::default(),
            price2_cumulative_last: BalanceOf::<T>::default(),
        };
        Ok((exchange, initial_shares))
    }

    fn perform_first_to_second_asset_swap_calculation(
        &self,
        exchange_fee: BalanceOf<T>,
        first_asset_amount: BalanceOf<T>,
    ) -> Result<SwapDelta<T>, Error<T>> {
        let new_first_asset_pool = self
            .first_asset_pool
            .checked_add(&first_asset_amount)
            .ok_or(Error::<T>::OverflowOccured)?;
        let temp_first_asset_pool = new_first_asset_pool
            .checked_sub(&exchange_fee)
            .ok_or(Error::<T>::UnderflowOccured)?;
        let new_second_asset_pool = self
            .invariant
            .checked_div(&temp_first_asset_pool)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
        let second_asset_amount = self
            .second_asset_pool
            .checked_sub(&new_second_asset_pool)
            .ok_or(Error::<T>::UnderflowOccured)?;

        Ok(SwapDelta::new(
            new_first_asset_pool,
            new_second_asset_pool,
            second_asset_amount,
        ))
    }

    pub fn calculate_first_to_second_asset_swap(
        &self,
        first_asset_amount: BalanceOf<T>,
    ) -> Result<(SwapDelta<T>, Option<(BalanceOf<T>, T::AccountId)>), Error<T>> {
        let fee = T::FeeRateNominator::get()
            .checked_mul(&first_asset_amount)
            .map(|result| result.checked_div(&T::FeeRateDenominator::get()))
            .flatten()
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        if let Ok(dex_treasury) = <DEXTreasury<T>>::try_get() {
            let treasury_fee = dex_treasury
                .treasury_fee_rate_nominator
                .checked_mul(&fee)
                .map(|result| result.checked_div(&dex_treasury.treasury_fee_rate_denominator))
                .flatten()
                .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
            let exchange_fee = fee - treasury_fee;
            let swap_delta = self
                .perform_first_to_second_asset_swap_calculation(exchange_fee, first_asset_amount)?;
            Ok((swap_delta, Some((treasury_fee, dex_treasury.dex_account))))
        } else {
            let swap_delta =
                self.perform_first_to_second_asset_swap_calculation(fee, first_asset_amount)?;
            Ok((swap_delta, None))
        }
    }

    fn perform_second_to_first_asset_swap_calculation(
        &self,
        exchange_fee: BalanceOf<T>,
        second_asset_amount: BalanceOf<T>,
    ) -> Result<SwapDelta<T>, Error<T>> {
        let new_second_asset_pool = self
            .second_asset_pool
            .checked_add(&second_asset_amount)
            .ok_or(Error::<T>::OverflowOccured)?;
        let temp_second_asset_pool = new_second_asset_pool
            .checked_sub(&exchange_fee)
            .ok_or(Error::<T>::UnderflowOccured)?;
        let new_first_asset_pool = self
            .invariant
            .checked_div(&temp_second_asset_pool)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
        let first_asset_amount = self
            .first_asset_pool
            .checked_sub(&new_first_asset_pool)
            .ok_or(Error::<T>::UnderflowOccured)?;

        Ok(SwapDelta::new(
            new_first_asset_pool,
            new_second_asset_pool,
            first_asset_amount,
        ))
    }

    pub fn calculate_second_to_first_asset_swap(
        &self,
        second_asset_amount: BalanceOf<T>,
    ) -> Result<(SwapDelta<T>, Option<(BalanceOf<T>, T::AccountId)>), Error<T>> {
        let fee = T::FeeRateNominator::get()
            .checked_mul(&second_asset_amount)
            .map(|result| result.checked_div(&T::FeeRateDenominator::get()))
            .flatten()
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        if let Ok(dex_treasury) = <DEXTreasury<T>>::try_get() {
            let treasury_fee = dex_treasury
                .treasury_fee_rate_nominator
                .checked_mul(&fee)
                .map(|result| result.checked_div(&dex_treasury.treasury_fee_rate_denominator))
                .flatten()
                .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
            let exchange_fee = fee - treasury_fee;
            let swap_delta = self.perform_second_to_first_asset_swap_calculation(
                exchange_fee,
                second_asset_amount,
            )?;
            Ok((swap_delta, Some((treasury_fee, dex_treasury.dex_account))))
        } else {
            let swap_delta =
                self.perform_second_to_first_asset_swap_calculation(fee, second_asset_amount)?;
            Ok((swap_delta, None))
        }
    }

    pub fn calculate_costs(
        &self,
        shares: BalanceOf<T>,
    ) -> Result<(BalanceOf<T>, BalanceOf<T>), Error<T>> {
        let first_asset_cost = shares
            .checked_div(&self.total_shares)
            .map(|ratio| ratio * self.first_asset_pool)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
        let second_asset_cost = shares
            .checked_div(&self.total_shares)
            .map(|ratio| ratio * self.second_asset_pool)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        Ok((first_asset_cost, second_asset_cost))
    }

    pub fn invest(
        &mut self,
        first_asset_amount: BalanceOf<T>,
        second_asset_amount: BalanceOf<T>,
        shares: BalanceOf<T>,
        sender: &T::AccountId,
    ) -> Result<(), Error<T>> {
        let updated_shares = if let Some(prev_shares) = self.shares.get(sender) {
            prev_shares
                .checked_add(&shares)
                .ok_or(Error::<T>::OverflowOccured)?
        } else {
            shares
        };

        self.shares.insert(sender.clone(), updated_shares);
        self.total_shares = self
            .total_shares
            .checked_add(&shares)
            .ok_or(Error::<T>::OverflowOccured)?;
        self.first_asset_pool = self
            .first_asset_pool
            .checked_add(&first_asset_amount)
            .ok_or(Error::<T>::OverflowOccured)?;
        self.second_asset_pool = self
            .second_asset_pool
            .checked_add(&second_asset_amount)
            .ok_or(Error::<T>::OverflowOccured)?;
        self.invariant = self
            .first_asset_pool
            .checked_mul(&self.second_asset_pool)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
        Ok(())
    }

    pub fn divest(
        &mut self,
        first_asset_amount: BalanceOf<T>,
        second_asset_amount: BalanceOf<T>,
        shares: BalanceOf<T>,
        sender: &T::AccountId,
    ) -> Result<(), Error<T>> {
        if let Some(share) = self.shares.get_mut(sender) {
            share
                .checked_sub(&shares)
                .ok_or(Error::<T>::UnderflowOccured)?;
        }

        self.total_shares = self
            .total_shares
            .checked_sub(&shares)
            .ok_or(Error::<T>::UnderflowOccured)?;
        self.first_asset_pool = self
            .first_asset_pool
            .checked_sub(&first_asset_amount)
            .ok_or(Error::<T>::UnderflowOccured)?;
        self.second_asset_pool = self
            .second_asset_pool
            .checked_sub(&second_asset_amount)
            .ok_or(Error::<T>::UnderflowOccured)?;
        if self.total_shares == BalanceOf::<T>::zero() {
            self.invariant = BalanceOf::<T>::zero();
        } else {
            self.invariant = self
                .first_asset_pool
                .checked_mul(&self.second_asset_pool)
                .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
        }
        Ok(())
    }

    pub fn update_pools(
        &mut self,
        first_asset_pool: BalanceOf<T>,
        second_asset_pool: BalanceOf<T>,
    ) -> Result<(), Error<T>> {
        self.first_asset_pool = first_asset_pool;
        self.second_asset_pool = second_asset_pool;

        let now: T::IMoment = <pallet_timestamp::Module<T>>::get().into();
        let time_elapsed: T::IMoment = now
            .checked_sub(&self.last_timestamp)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        let price1_cumulative = first_asset_pool
            .checked_div(&second_asset_pool)
            .map(|result| result.checked_mul(&time_elapsed.into()))
            .flatten()
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        self.price1_cumulative_last = self
            .price1_cumulative_last
            .checked_add(&price1_cumulative)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        let price2_cumulative = second_asset_pool
            .checked_div(&first_asset_pool)
            .map(|result| result.checked_mul(&time_elapsed.into()))
            .flatten()
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        self.price2_cumulative_last = self
            .price2_cumulative_last
            .checked_add(&price2_cumulative)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;

        self.last_timestamp = now;

        self.invariant = self
            .first_asset_pool
            .checked_mul(&self.second_asset_pool)
            .ok_or(Error::<T>::UnderflowOrOverflowOccured)?;
        Ok(())
    }

    pub fn ensure_launch(&self) -> dispatch::DispatchResult {
        ensure!(
            self.invariant == BalanceOf::<T>::zero(),
            Error::<T>::InvariantNotNull
        );
        ensure!(
            self.total_shares == BalanceOf::<T>::zero(),
            Error::<T>::TotalSharesNotNull
        );
        Ok(())
    }

    pub fn ensure_second_asset_amount(
        &self,
        asset_out_amount: BalanceOf<T>,
        min_asset_out_amount: BalanceOf<T>,
    ) -> dispatch::DispatchResult {
        ensure!(
            asset_out_amount >= min_asset_out_amount,
            Error::<T>::SecondAssetAmountBelowExpectation
        );
        ensure!(
            asset_out_amount <= self.second_asset_pool,
            Error::<T>::InsufficientPool
        );
        Ok(())
    }

    pub fn ensure_burned_shares(
        &self,
        sender: &T::AccountId,
        shares_burned: BalanceOf<T>,
    ) -> dispatch::DispatchResult {
        ensure!(
            shares_burned > BalanceOf::<T>::zero(),
            Error::<T>::InvalidShares
        );
        if let Some(shares) = self.shares.get(sender) {
            ensure!(*shares >= shares_burned, Error::<T>::InsufficientShares);
            Ok(())
        } else {
            Err(Error::<T>::DoesNotOwnShare.into())
        }
    }

    pub fn ensure_first_asset_amount(
        &self,
        first_asset_out_amount: BalanceOf<T>,
        min_first_asset_out_amount: BalanceOf<T>,
    ) -> dispatch::DispatchResult {
        ensure!(
            first_asset_out_amount >= min_first_asset_out_amount,
            Error::<T>::SecondAssetAmountBelowExpectation
        );
        ensure!(
            first_asset_out_amount <= self.first_asset_pool,
            Error::<T>::InsufficientPool
        );
        Ok(())
    }
}
