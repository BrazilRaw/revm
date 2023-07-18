use super::{
    reverts::AccountInfoRevert, AccountRevert, AccountStatus, RevertToSlot,
    StorageWithOriginalValues, TransitionAccount,
};
use revm_interpreter::primitives::{AccountInfo, StorageSlot, U256};
use revm_precompile::HashMap;

/// Account information focused on creating of database changesets
/// and Reverts.
///
/// Status is needed as to know from what state we are applying the TransitionAccount.
///
/// Original account info is needed to know if there was a change.
/// Same thing for storage with original value.
///
/// On selfdestruct storage original value is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleAccount {
    pub info: Option<AccountInfo>,
    pub original_info: Option<AccountInfo>,
    /// Contain both original and present state.
    /// When extracting changeset we compare if original value is different from present value.
    /// If it is different we add it to changeset.
    ///
    /// If Account was destroyed we ignore original value and comprate present state with U256::ZERO.
    pub storage: StorageWithOriginalValues,
    /// Account status.
    pub status: AccountStatus,
}

impl BundleAccount {
    /// Create new BundleAccount.
    pub fn new(
        original_info: Option<AccountInfo>,
        present_info: Option<AccountInfo>,
        storage: StorageWithOriginalValues,
        status: AccountStatus,
    ) -> Self {
        Self {
            info: present_info,
            original_info,
            storage,
            status,
        }
    }

    /// Return storage slot if it exist.
    ///
    /// In case we know that account is destroyed return `Some(U256::ZERO)`
    pub fn storage_slot(&self, slot: U256) -> Option<U256> {
        let slot = self.storage.get(&slot).map(|s| s.present_value);
        if slot.is_some() {
            slot
        } else if self.status.storage_known() {
            Some(U256::ZERO)
        } else {
            None
        }
    }

    /// Fetch account info if it exist.
    pub fn account_info(&self) -> Option<AccountInfo> {
        self.info.clone()
    }

    /// Was this account destroyed.
    pub fn was_destroyed(&self) -> bool {
        self.status.was_destroyed()
    }

    /// Return true of account info was changed.
    pub fn is_info_changed(&self) -> bool {
        self.info != self.original_info
    }

    /// Return true if contract was changed
    pub fn is_contract_changed(&self) -> bool {
        self.info.as_ref().map(|a| a.code_hash) != self.original_info.as_ref().map(|a| a.code_hash)
    }

    /// Revert account to previous state and return true if account can be removed.
    pub fn revert(&mut self, revert: AccountRevert) -> bool {
        self.status = revert.previous_status;

        match revert.account {
            AccountInfoRevert::DoNothing => (),
            AccountInfoRevert::DeleteIt => {
                self.info = None;
                self.storage = HashMap::new();
                return true;
            }
            AccountInfoRevert::RevertTo(info) => self.info = Some(info),
        };
        // revert stoarge
        for (key, slot) in revert.storage {
            match slot {
                RevertToSlot::Some(value) => {
                    // Dont overwrite original values if present
                    // if storage is not present set original values as currect value.
                    self.storage
                        .entry(key)
                        .or_insert(StorageSlot::new_changed(value, U256::ZERO))
                        .present_value = value;
                }
                RevertToSlot::Destroyed => {
                    // if it was destroyed this means that storage was created and we need to remove it.
                    self.storage.remove(&key);
                }
            }
        }
        false
    }

    /// Extend account with another account.
    ///
    /// It is similar with the update but it is done with another BundleAccount.
    ///
    /// Original values of acccount and storage stay the same.
    pub(crate) fn extend(&mut self, other: Self) {
        self.status = other.status;
        self.info = other.info;
        // extend storage
        for (key, storage_slot) in other.storage {
            // update present value or insert storage slot.
            self.storage
                .entry(key)
                .or_insert(storage_slot)
                .present_value = storage_slot.present_value;
        }
    }

    /// Update to new state and generate AccountRevert that if applied to new state will
    /// revert it to previous state. If no revert is present, update is noop.
    pub fn update_and_create_revert(
        &mut self,
        transition: TransitionAccount,
    ) -> Option<AccountRevert> {
        let updated_info = transition.info;
        let updated_storage = transition.storage;
        let updated_status = transition.status;

        // the helper that extends this storage but preserves original value.
        let extend_storage =
            |this_storage: &mut StorageWithOriginalValues,
             storage_update: StorageWithOriginalValues| {
                for (key, value) in storage_update {
                    this_storage.entry(key).or_insert(value).present_value = value.present_value;
                }
            };

        // handle it more optimal in future but for now be more flexible to set the logic.
        let previous_storage_from_update = updated_storage
            .iter()
            .filter(|s| s.1.original_value != s.1.present_value)
            .map(|(key, value)| (*key, RevertToSlot::Some(value.original_value)))
            .collect();

        match updated_status {
            AccountStatus::Changed => {
                match self.status {
                    AccountStatus::Changed => {
                        // extend the storage. original values is not used inside bundle.
                        let revert_info = if self.info != updated_info {
                            AccountInfoRevert::RevertTo(self.info.clone().unwrap_or_default())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        extend_storage(&mut self.storage, updated_storage);
                        self.info = updated_info;
                        Some(AccountRevert {
                            account: revert_info,
                            storage: previous_storage_from_update,
                            previous_status: AccountStatus::Changed,
                            wipe_storage: false,
                        })
                    }
                    AccountStatus::Loaded => {
                        let info_revert = if self.info != updated_info {
                            AccountInfoRevert::RevertTo(self.info.clone().unwrap_or_default())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        self.status = AccountStatus::Changed;
                        self.info = updated_info;
                        extend_storage(&mut self.storage, updated_storage);

                        Some(AccountRevert {
                            account: info_revert,
                            storage: previous_storage_from_update,
                            previous_status: AccountStatus::Loaded,
                            wipe_storage: false,
                        })
                    }
                    AccountStatus::LoadedEmptyEIP161 => {
                        // Only change that can happen from LoadedEmpty to Changed
                        // is if balance is send to account. So we are only checking account change here.
                        let info_revert = if self.info != updated_info {
                            AccountInfoRevert::RevertTo(self.info.clone().unwrap_or_default())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        self.status = AccountStatus::Changed;
                        self.info = updated_info;
                        Some(AccountRevert {
                            account: info_revert,
                            storage: HashMap::default(),
                            previous_status: AccountStatus::Loaded,
                            wipe_storage: false,
                        })
                    }
                    _ => unreachable!("Invalid state transfer to Changed from {self:?}"),
                }
            }
            AccountStatus::InMemoryChange => match self.status {
                AccountStatus::LoadedEmptyEIP161 => {
                    let revert_info = if self.info != updated_info {
                        AccountInfoRevert::RevertTo(AccountInfo::default())
                    } else {
                        AccountInfoRevert::DoNothing
                    };
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::InMemoryChange;
                    self.info = updated_info;
                    extend_storage(&mut self.storage, updated_storage);

                    Some(AccountRevert {
                        account: revert_info,
                        storage: previous_storage_from_update,
                        previous_status: AccountStatus::LoadedEmptyEIP161,
                        wipe_storage: false,
                    })
                }
                AccountStatus::Loaded => {
                    // from loaded to InMemoryChange can happen if there is balance change
                    // or new created account but Loaded didn't have contract.
                    let revert_info = if self.info != updated_info {
                        AccountInfoRevert::RevertTo(AccountInfo::default())
                    } else {
                        AccountInfoRevert::DoNothing
                    };
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::InMemoryChange;
                    self.info = updated_info;
                    extend_storage(&mut self.storage, updated_storage);

                    Some(AccountRevert {
                        account: revert_info,
                        storage: previous_storage_from_update,
                        previous_status: AccountStatus::Loaded,
                        wipe_storage: false,
                    })
                }
                AccountStatus::LoadedNotExisting => {
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::InMemoryChange;
                    self.info = updated_info;
                    self.storage = updated_storage;

                    Some(AccountRevert {
                        account: AccountInfoRevert::DeleteIt,
                        storage: previous_storage_from_update,
                        previous_status: AccountStatus::LoadedNotExisting,
                        wipe_storage: false,
                    })
                }
                AccountStatus::InMemoryChange => {
                    let revert_info = if self.info != updated_info {
                        AccountInfoRevert::RevertTo(self.info.clone().unwrap_or_default())
                    } else {
                        AccountInfoRevert::DoNothing
                    };
                    // set as new as we didn't have that transition
                    self.status = AccountStatus::InMemoryChange;
                    self.info = updated_info;
                    extend_storage(&mut self.storage, updated_storage);

                    Some(AccountRevert {
                        account: revert_info,
                        storage: previous_storage_from_update,
                        previous_status: AccountStatus::InMemoryChange,
                        wipe_storage: false,
                    })
                }
                _ => unreachable!("Invalid change to InMemoryChange from {self:?}"),
            },
            AccountStatus::Loaded
            | AccountStatus::LoadedNotExisting
            | AccountStatus::LoadedEmptyEIP161 => {
                // No changeset, maybe just update data
                // Do nothing for now.
                None
            }
            AccountStatus::Destroyed => {
                let this_info = self.info.take().unwrap_or_default();
                let this_storage = self.storage.drain().collect();
                let ret = match self.status {
                    AccountStatus::InMemoryChange | AccountStatus::Changed | AccountStatus::Loaded | AccountStatus::LoadedEmptyEIP161 => {
                        AccountRevert::new_selfdestructed(self.status, this_info, this_storage)
                    }
                    AccountStatus::LoadedNotExisting => {
                        // Do nothing as we have LoadedNotExisting -> Destroyed (It is noop)
                        return None;
                    }
                    _ => unreachable!("Invalid transition to Destroyed account from: {self:?} to {updated_info:?} {updated_status:?}"),
                };
                self.status = AccountStatus::Destroyed;
                // set present to destroyed.
                Some(ret)
            }
            AccountStatus::DestroyedChanged => {
                // Previous block created account or changed.
                // (It was destroyed on previous block or one before).

                // check common pre destroy paths.
                if let Some(revert_state) =
                    AccountRevert::new_selfdestructed_from_bundle(self, &updated_storage)
                {
                    // set to destroyed and revert state.
                    self.status = AccountStatus::DestroyedChanged;
                    self.info = updated_info;
                    self.storage = updated_storage;

                    return Some(revert_state);
                }

                let ret = match self.status {
                    AccountStatus::Destroyed => {
                        // from destroyed state new account is made
                        Some(AccountRevert {
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            previous_status: AccountStatus::Destroyed,
                            wipe_storage: false,
                        })
                    }
                    AccountStatus::DestroyedChanged => {
                        let revert_info = if self.info != updated_info {
                            AccountInfoRevert::RevertTo(AccountInfo::default())
                        } else {
                            AccountInfoRevert::DoNothing
                        };
                        // Stays same as DestroyedNewChanged
                        Some(AccountRevert {
                            // empty account
                            account: revert_info,
                            storage: previous_storage_from_update,
                            previous_status: AccountStatus::DestroyedChanged,
                            wipe_storage: false,
                        })
                    }
                    AccountStatus::LoadedNotExisting => {
                        Some(AccountRevert {
                            // empty account
                            account: AccountInfoRevert::DeleteIt,
                            storage: previous_storage_from_update,
                            previous_status: AccountStatus::LoadedNotExisting,
                            wipe_storage: false,
                        })
                    }
                    AccountStatus::DestroyedAgain => Some(AccountRevert::new_selfdestructed_again(
                        // destroyed again will set empty account.
                        AccountStatus::DestroyedAgain,
                        AccountInfo::default(),
                        HashMap::default(),
                        updated_storage.clone(),
                    )),
                    _ => unreachable!("Invalid state transfer to DestroyedNew from {self:?}"),
                };
                self.status = AccountStatus::DestroyedChanged;
                self.info = updated_info;
                self.storage = updated_storage;

                ret
            }
            AccountStatus::DestroyedAgain => {
                // Previous block created account
                // (It was destroyed on previous block or one before).

                // check common pre destroy paths.
                let ret = if let Some(revert_state) =
                    AccountRevert::new_selfdestructed_from_bundle(self, &HashMap::default())
                {
                    Some(revert_state)
                } else {
                    match self.status {
                        AccountStatus::Destroyed
                        | AccountStatus::DestroyedAgain
                        | AccountStatus::LoadedNotExisting => {
                            // From destroyed to destroyed again. is noop
                            //
                            // DestroyedAgain to DestroyedAgain is noop
                            //
                            // From LoadedNotExisting to DestroyedAgain
                            // is noop as account is destroyed again
                            None
                        }
                        AccountStatus::DestroyedChanged => {
                            // From destroyed new to destroyed again.
                            let ret = AccountRevert {
                                // empty account
                                account: AccountInfoRevert::RevertTo(
                                    self.info.clone().unwrap_or_default(),
                                ),
                                // TODO(rakita) is this invalid?
                                storage: previous_storage_from_update,
                                previous_status: AccountStatus::DestroyedChanged,
                                wipe_storage: false,
                            };
                            Some(ret)
                        }
                        _ => unreachable!("Invalid state to DestroyedAgain from {self:?}"),
                    }
                };
                // set to destroyed and revert state.
                self.status = AccountStatus::DestroyedAgain;
                self.info = None;
                self.storage.clear();
                ret
            }
        }
    }
}
