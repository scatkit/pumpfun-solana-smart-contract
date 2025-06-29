use crate::{
    constants::{BONDING_CURVE, CONFIG, GLOBAL, METADATA},
    errors::*,
    events::LaunchEvent,
    state::{bondingcurve::*, config::*},
};
use anchor_lang::{prelude::*, solana_program::sysvar::SysvarId, system_program};
use anchor_spl::{
    associated_token::{self, AssociatedToken},
    metadata::{self, mpl_token_metadata::types::DataV2, Metadata},
    token::{self, spl_token::instruction::AuthorityType, Mint, Token},
};

#[derive(Accounts)]
#[instruction(decimals: u8)]
pub struct CreateBondingCurve<'info> {
    #[account(
        mut,
        seeds = [CONFIG.as_bytes()],
        bump,
    )]
    global_config: Box<Account<'info, Config>>,

    /// CHECK: global vault pda which stores SOL
    #[account(
        mut,
        seeds = [GLOBAL.as_bytes()],
        bump,
    )]
    pub global_vault: AccountInfo<'info>,

    /// User calling the instruction
    #[account(mut)]
    creator: Signer<'info>,

    #[account(
        init,
        payer = creator,
        mint::decimals = decimals,
        mint::authority = global_vault.key(),
    )]
    token: Box<Account<'info, Mint>>,

    #[account(
        init,
        payer = creator,
        space = 8 + std::mem::size_of::<BondingCurve>(),
        seeds = [BONDING_CURVE.as_bytes(), &token.key().to_bytes()],
        bump
    )]
    bonding_curve: Box<Account<'info, BondingCurve>>,

    /// CHECK: passed to token metadata program
    #[account(
        mut,
        seeds = [
            METADATA.as_bytes(),
            metadata::ID.as_ref(), // metaplex address 
            token.key().as_ref(),
        ],
        bump,
        seeds::program = metadata::ID
    )]
    token_metadata_account: UncheckedAccount<'info>, // PDA account

    /// CHECK: created in instruction
    #[account(
        mut,
        seeds = [
            global_vault.key().as_ref(),
            token::spl_token::ID.as_ref(),
            token.key().as_ref(),
        ],
        bump,
        seeds::program = associated_token::ID
    )]
    global_token_account: UncheckedAccount<'info>, // ATA to hold new tokens

    #[account(address = system_program::ID)]
    system_program: Program<'info, System>,

    #[account(address = Rent::id())]
    rent: Sysvar<'info, Rent>,

    #[account(address = token::ID)]
    token_program: Program<'info, Token>,

    #[account(address = associated_token::ID)]
    associated_token_program: Program<'info, AssociatedToken>,

    #[account(address = metadata::ID)]
    mpl_token_metadata_program: Program<'info, Metadata>,

    /// CHECK: should be same with the address in the global_config
    #[account(
        mut,
        constraint = global_config.team_wallet == team_wallet.key() @ContractError::IncorrectAuthority
    )]
    pub team_wallet: AccountInfo<'info>,
}

impl<'info> CreateBondingCurve<'info> {
    pub fn handler(
        &mut self,

        // launch config
        decimals: u8,
        token_supply: u64,
        reserve_lamport: u64,

        // metadata
        name: String,
        symbol: String,
        uri: String,
        global_vault_bump: u8,
    ) -> Result<()> {
        let global_config = &self.global_config;
        let creator = &self.creator;
        let token = &self.token;
        let global_token_account = &self.global_token_account; // ata
        let bonding_curve = &mut self.bonding_curve; // pda
        let global_vault = &self.global_vault;

        //  check params
        let decimal_multiplier = 10u64.pow(decimals as u32); // 10^6 = 1_000_000
        let fractional_tokens = token_supply % decimal_multiplier;
        if fractional_tokens != 0 {
            return Err(ValueInvalid.into());
        }

        // check wether it meets min/max treshold:
        global_config
            .lamport_amount_config
            .validate(&reserve_lamport)?;

        global_config
            .token_supply_config
            .validate(&(token_supply / decimal_multiplier))?;

        global_config.token_decimals_config.validate(&decimals)?;
        //

        // create token launch pda:
        // pub struct BondingCurve {
        //     pub token_mint: Pubkey,
        //     pub creator: Pubkey,
        //     pub init_lamport: u64,
        //     pub token_total_supply: u64,
        //     pub virtual_sol_reserves: u64,
        //     pub virtual_token_reserves: u64,
        //     pub real_sol_reserves: u64,
        //     pub real_token_reserves: u64,
        //     pub is_completed: bool,
        // }
        bonding_curve.token_mint = token.key();
        bonding_curve.creator = creator.key();
        bonding_curve.init_lamport = reserve_lamport; // ???

        bonding_curve.virtual_sol_reserves = global_config.initial_virtual_sol_reserves_config;
        bonding_curve.virtual_token_reserves = global_config.initial_virtual_token_reserves_config;
        bonding_curve.real_sol_reserves = 0;
        bonding_curve.real_token_reserves = global_config.initial_real_token_reserves_config;
        bonding_curve.token_total_supply = token_supply; // 1B

        // create global token account (for the bonding curve to hold tokens)
        associated_token::create(CpiContext::new(
            self.associated_token_program.to_account_info(), // specify the program to be invoked
            associated_token::Create {
                // accounts required by the instruction:
                payer: creator.to_account_info(), // pays rent and creation fees
                associated_token: global_token_account.to_account_info(), // ata being created
                authority: global_vault.to_account_info(), // owner of the ata
                mint: token.to_account_info(),    // token mint this ata is tied to
                token_program: self.token_program.to_account_info(), // required by the ATA program to call into the SPL token program to initialize the ATA
                system_program: self.system_program.to_account_info(), // required to create the account on-chain (it's the only program that can create new system accounts)
            },
        ))?;

        let signer_seeds: &[&[&[u8]]] = &[&[GLOBAL.as_bytes(), &[global_vault_bump]]];

        // mint tokens to bonding curve & team
        token::mint_to(
            CpiContext::new_with_signer(
                self.token_program.to_account_info(),
                token::MintTo {
                    mint: token.to_account_info(),
                    to: global_token_account.to_account_info(),
                    authority: global_vault.to_account_info(),
                },
                signer_seeds,
            ),
            token_supply, // mints (e.g 1B tokens)
        )?;

        // create metadata
        metadata::create_metadata_accounts_v3(
            CpiContext::new_with_signer(
                self.mpl_token_metadata_program.to_account_info(), // program to be invoked
                metadata::CreateMetadataAccountsV3 {
                    metadata: self.token_metadata_account.to_account_info(), // metadata itself
                    mint: token.to_account_info(), // the token this metadata is tied to
                    mint_authority: global_vault.to_account_info(),
                    payer: creator.to_account_info(),
                    update_authority: global_vault.to_account_info(),
                    system_program: self.system_program.to_account_info(),
                    rent: self.rent.to_account_info(),
                },
                signer_seeds,
            ),
            DataV2 {
                name,
                symbol,
                uri,
                seller_fee_basis_points: 0,
                creators: None,
                collection: None,
                uses: None,
            },
            false,
            true,
            None,
        )?;

        //  revoke mint authority
        token::set_authority(
            CpiContext::new_with_signer(
                self.token_program.to_account_info(),
                token::SetAuthority {
                    current_authority: global_vault.to_account_info(),
                    account_or_mint: token.to_account_info(),
                },
                signer_seeds,
            ),
            AuthorityType::MintTokens,
            None,
        )?;

        bonding_curve.is_completed = false;

        emit!(LaunchEvent {
            creator: self.creator.key(),
            mint: self.token.key(),
            bonding_curve: self.bonding_curve.key(),
            metadata: self.token_metadata_account.key(),
            decimals,
            token_supply,
            reserve_lamport,
            reserve_token: global_config.initial_real_token_reserves_config
        });

        Ok(())
    }
}
