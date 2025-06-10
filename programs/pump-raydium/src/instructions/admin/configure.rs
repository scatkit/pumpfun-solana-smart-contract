use crate::errors::*;
use crate::{
    constants::{CONFIG, GLOBAL},
    state::config::*,
    utils::sol_transfer_from_user,
};
use anchor_lang::{prelude::*, system_program, Discriminator};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{Mint, Token, TokenAccount},
};
use borsh::BorshDeserialize;

#[derive(Accounts)]
/// *what accs the instruction requires
pub struct Configure<'info> {
    #[account(mut)]
    payer: Signer<'info>, // creator

    /// CHECK: initialization handled inside the instruction ( Anchor still resolves the PDA address )
    #[account(
        mut,
        seeds = [CONFIG.as_bytes()],
        bump,
    )]
    config: AccountInfo<'info>, // PDA that stores raw bytes containing the serialized `new_config`

    /// CHECK: global vault pda which stores SOL
    #[account(
        mut,
        seeds = [GLOBAL.as_bytes()],
        bump,
    )]
    pub global_vault: AccountInfo<'info>,

    #[account(
        init_if_needed,
        payer = payer, 
        associated_token::mint = native_mint,
        associated_token::authority = global_vault
    )]
    global_wsol_account: Box<Account<'info, TokenAccount>>, // holds WSOL conrolled by global_vault

    #[account(
        address = spl_token::native_mint::ID
    )]
    native_mint: Box<Account<'info, Mint>>, // for mint operations witth WSOL

    #[account(address = system_program::ID)]
    system_program: Program<'info, System>, // for creating accounts, transferring lamports

    token_program: Program<'info, Token>, // for minting, transfering tokens, other SPL tokens

    associated_token_program: Program<'info, AssociatedToken>,
}

impl<'info> Configure<'info> {
    pub fn handler(&mut self, new_config: Config, config_bump: u8) -> Result<()> {
        let serialized_config =
            [&Config::DISCRIMINATOR, new_config.try_to_vec()?.as_slice()].concat(); // 8 byte Anhcor desriminator + serialized new_config
        let serialized_config_len = serialized_config.len();
        let config_cost = Rent::get()?.minimum_balance(serialized_config_len);

        //  init config pda
        if self.config.owner != &crate::ID { // if config PDA hasn't been initialized 
            let cpi_context = CpiContext::new( // specifies which accounts are involved
                self.system_program.to_account_info(),
                system_program::CreateAccount { // system program requires these two fields:
                    from: self.payer.to_account_info(), // who funds the new account
                    to: self.config.to_account_info(), // the new acc to be created 
                },
            );
            system_program::create_account(
                // I am asking the system_program to init this PDA
                // Only the my program (smart contract address) that owns the PDA can sign for it.
                // it provides the PDA seeds to prove it
                cpi_context.with_signer(&[&[CONFIG.as_bytes(), &[config_bump]]]), 
                config_cost,
                serialized_config_len as u64,
                &crate::ID,
            )?;
        } else {
            // validate the existing config if already initialized
            let data = self.config.try_borrow_data()?;
            if data.len() < 8 || &data[0..8] != Config::DISCRIMINATOR { // ensure that the descriminator (first 8 bytes) matches
                return err!(ContractError::IncorrectConfigAccount);
            }
            let config = Config::deserialize(&mut &data[8..])?;

            if config.authority != self.payer.key() {
                return err!(ContractError::IncorrectAuthority);
            }
        }

        let lamport_delta = (config_cost as i64) - (self.config.lamports() as i64); 
        if lamport_delta > 0 {  // top up rent if needed
            system_program::transfer(
                CpiContext::new(
                    self.system_program.to_account_info(),
                    system_program::Transfer {
                        from: self.payer.to_account_info(), // payer
                        to: self.config.to_account_info(),  // receiver
                    },
                ),
                lamport_delta as u64,
            )?;
            self.config.realloc(serialized_config_len, false)?; // This resizes the config account’s data buffer to fit the new serialized config.
        }

        (self.config.try_borrow_mut_data()?[..serialized_config_len]) // write serizalied bytes (including the descriminator) into the config account's data buffer
            .copy_from_slice(serialized_config.as_slice());

        //  initialize global vault if it hasn't been
        if self.global_vault.lamports() == 0 {
            sol_transfer_from_user(
                &self.payer,
                self.global_vault.clone(),
                &self.system_program,
                1000000,
            )?;
        }
        Ok(())
    }
}
