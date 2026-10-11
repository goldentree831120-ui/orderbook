//! Shared LiteSVM harness: load the compiled program, send instructions, read the market,
//! decode Anchor events from logs, and report compute units.
#![allow(dead_code)]

use anchor_lang::{
    prelude::Pubkey,
    solana_program::{instruction::Instruction, system_program},
    AnchorDeserialize, Discriminator, InstructionData, ToAccountMetas,
};
use base64::Engine;
use litesvm::{types::TransactionMetadata, LiteSVM};
use onchain_book::{Market, TradeEvent, MARKET_SEED};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

pub struct Env {
    pub svm: LiteSVM,
    pub authority: Keypair,
    pub market: Pubkey,
}

#[derive(Debug)]
pub struct Sent {
    pub cu: u64,
    pub logs: Vec<String>,
}

pub fn setup() -> Env {
    let mut svm = LiteSVM::new();
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/deploy/onchain_book.so"
    ));
    svm.add_program(onchain_book::id(), bytes).unwrap();
    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();
    let market = Pubkey::find_program_address(
        &[MARKET_SEED, authority.pubkey().as_ref()],
        &onchain_book::id(),
    )
    .0;
    let mut env = Env {
        svm,
        authority,
        market,
    };
    let ix = Instruction::new_with_bytes(
        onchain_book::id(),
        &onchain_book::instruction::InitializeMarket {}.data(),
        onchain_book::accounts::InitializeMarket {
            authority: env.authority.pubkey(),
            market,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    let payer = env.authority.insecure_clone();
    env.send(ix, &payer).expect("initialize_market");
    env
}

impl Env {
    pub fn user(&mut self) -> Keypair {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), 1_000_000_000).unwrap();
        k
    }

    pub fn send(&mut self, ix: Instruction, signer: &Keypair) -> Result<Sent, String> {
        self.svm.expire_blockhash(); // every tx unique, even if identical to the previous one
        let msg = Message::new_with_blockhash(
            &[ix],
            Some(&signer.pubkey()),
            &self.svm.latest_blockhash(),
        );
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[signer]).unwrap();
        match self.svm.send_transaction(tx) {
            Ok(meta) => Ok(sent(meta)),
            Err(e) => Err(format!("{:?} | logs: {}", e.err, e.meta.logs.join(" / "))),
        }
    }

    pub fn place(
        &mut self,
        owner: &Keypair,
        side: u8,
        price: u64,
        qty: u64,
    ) -> Result<Sent, String> {
        let ix = Instruction::new_with_bytes(
            onchain_book::id(),
            &onchain_book::instruction::PlaceOrder { side, price, qty }.data(),
            onchain_book::accounts::PlaceOrder {
                market: self.market,
                owner: owner.pubkey(),
            }
            .to_account_metas(None),
        );
        self.send(ix, owner)
    }

    pub fn cancel(&mut self, owner: &Keypair, order_id: u64) -> Result<Sent, String> {
        let ix = Instruction::new_with_bytes(
            onchain_book::id(),
            &onchain_book::instruction::CancelOrder { order_id }.data(),
            onchain_book::accounts::CancelOrder {
                market: self.market,
                owner: owner.pubkey(),
            }
            .to_account_metas(None),
        );
        self.send(ix, owner)
    }

    /// Anyone can crank; `cranker` just pays the fee.
    pub fn match_orders(&mut self, cranker: &Keypair, max_fills: u16) -> Result<Sent, String> {
        let ix = Instruction::new_with_bytes(
            onchain_book::id(),
            &onchain_book::instruction::MatchOrders { max_fills }.data(),
            onchain_book::accounts::MatchOrders {
                market: self.market,
            }
            .to_account_metas(None),
        );
        self.send(ix, cranker)
    }

    pub fn market(&self) -> Market {
        let acc = self.svm.get_account(&self.market).unwrap();
        assert_eq!(&acc.data[..8], Market::DISCRIMINATOR);
        *bytemuck::from_bytes::<Market>(&acc.data[8..])
    }

    pub fn account_len(&self) -> usize {
        self.svm.get_account(&self.market).unwrap().data.len()
    }
}

fn sent(meta: TransactionMetadata) -> Sent {
    Sent {
        cu: meta.compute_units_consumed,
        logs: meta.logs,
    }
}

/// Decode every TradeEvent from a transaction's logs ("Program data: <base64>").
pub fn trades(s: &Sent) -> Vec<TradeEvent> {
    s.logs
        .iter()
        .filter_map(|l| l.strip_prefix("Program data: "))
        .filter_map(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .filter(|d| d.starts_with(TradeEvent::DISCRIMINATOR))
        .map(|d| TradeEvent::deserialize(&mut &d[8..]).unwrap())
        .collect()
}

pub fn error_name(err: &str) -> &str {
    for name in [
        "ZeroPrice",
        "ZeroQuantity",
        "InvalidSide",
        "BookFull",
        "OrderNotFound",
        "NotOwner",
        "InvalidMaxFills",
    ] {
        if err.contains(name) {
            return name;
        }
    }
    err
}
