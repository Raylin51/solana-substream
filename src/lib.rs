use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use substreams::log;
use substreams::store::{
    StoreAdd, StoreAddInt64, StoreGet, StoreGetInt64, StoreGetString, StoreNew, StoreSet,
    StoreSetString,
};
use substreams_entity_change::pb::entity::{value, EntityChanges};
use substreams_entity_change::tables::Tables;
use substreams_solana::pb::sf::solana::r#type::v1::{
    Block, CompiledInstruction, Message, Transaction, TransactionStatusMeta,
};

const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const RAYDIUM_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

const BUY_DISCRIMINATOR: &str = "66063d1201daebea";
const SELL_DISCRIMINATOR: &str = "33e685a4017f83ad";

#[derive(Debug, Clone)]
struct Trade {
    token_address: String,
    amount: i64,
    trade_type: String,
    timestamp: i64,
}

struct InstructionFilters {
    program_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WindowStats {
    window_start: i64,
    net_amount: i64,
    total_volume: i64,
    trade_count: i64,
}

#[substreams::handlers::map]
fn map_block_with_filtered_trades(
    params: String,
    mut block: Block,
) -> Result<Block, substreams::errors::Error> {
    let filters = parse_filters_from_params(params)?;

    // Retain only transactions that contain instructions matching our filters
    block.transactions.retain(|trx| {
        // Skip failed transactions
        match trx.meta.as_ref() {
            Some(meta) => {
                if meta.err.is_some() {
                    return false;
                }
                meta
            }
            None => return false,
        };

        let transaction = match trx.transaction.as_ref() {
            Some(transaction) => transaction,
            None => return false,
        };

        let message = match transaction.message.as_ref() {
            Some(message) => message,
            None => return false,
        };

        // Get all account keys for the transaction
        let account_keys = message.account_keys.iter().collect::<Vec<_>>();

        // Check if any instruction in this transaction matches our filters
        message
            .instructions
            .iter()
            .any(|inst| apply_filter(inst, &filters, &account_keys))
    });

    Ok(block)
}

#[substreams::handlers::store]
pub fn store_token_creations(blk: Block, store: StoreSetString) {
    let timestamp = blk
        .block_time
        .as_ref()
        .map(|t| t.timestamp)
        .unwrap_or_default();

    for tx in blk.transactions {
        if let Some(transaction) = tx.transaction {
            if let Some(message) = transaction.message {
                // 遍历所有指令
                for instruction in message.instructions {
                    // 检查程序 ID
                    let program_idx = instruction.program_id_index as usize;
                    if program_idx >= message.account_keys.len() {
                        continue;
                    }

                    let program_id = bs58::encode(&message.account_keys[program_idx]).into_string();
                    if program_id != PUMP_PROGRAM {
                        continue;
                    }

                    // 检查是否是创建方法
                    let data = &instruction.data;
                    if data.len() >= 8 {
                        let method_id = &data[..8];
                        if method_id == hex::decode("181ec828051c0777").unwrap().as_slice() {
                            // 获取 mint 地址（第一个账户）
                            if let Some(mint_index) = instruction.accounts.get(0) {
                                let mint_index = *mint_index as usize;
                                if mint_index < message.account_keys.len() {
                                    let mint = bs58::encode(&message.account_keys[mint_index])
                                        .into_string();
                                    log::debug!("Found create method, storing token: {}", mint);
                                    store.set(
                                        0,
                                        &format!("token:{}", mint),
                                        &timestamp.to_string(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[substreams::handlers::map]
pub fn map_trades(
    blk: Block,
    token_store: StoreGetString,
) -> Result<EntityChanges, substreams::errors::Error> {
    let mut tables = Tables::new();
    let timestamp = blk
        .block_time
        .as_ref()
        .map(|t| t.timestamp)
        .unwrap_or_default();

    // 收集当前区块的交易数据
    for tx in &blk.transactions {
        if let (Some(transaction), Some(meta)) = (tx.transaction.as_ref(), tx.meta.as_ref()) {
            if let Some(message) = transaction.message.as_ref() {
                // 处理主 instructions
                for instruction in &message.instructions {
                    if let Some(trade) = process_instruction_for_trades(
                        instruction,
                        message,
                        meta,
                        &token_store,
                        timestamp,
                        transaction,
                    )? {
                        process_trade(&mut tables, trade, timestamp, &token_store)?;
                    }
                }

                // 处理 inner instructions
                for inner_instruction in &meta.inner_instructions {
                    for instruction in &inner_instruction.instructions {
                        let compiled = CompiledInstruction {
                            program_id_index: instruction.program_id_index,
                            accounts: instruction.accounts.clone(),
                            data: instruction.data.clone(),
                        };
                        if let Some(trade) = process_instruction_for_trades(
                            &compiled,
                            message,
                            meta,
                            &token_store,
                            timestamp,
                            transaction,
                        )? {
                            process_trade(&mut tables, trade, timestamp, &token_store)?;
                        }
                    }
                }
            }
        }
    }

    Ok(tables.to_entity_changes())
}

// 4. 存 5m 的净交易量和总交易量
#[substreams::handlers::store]
pub fn store_5m_volume(changes: EntityChanges, store: StoreAddInt64) {
    for change in changes.entity_changes {
        if change.entity != "volume_5m" {
            continue;
        }

        if let Some(amount_field) = change.fields.iter().find(|f| f.name == "amount") {
            if let Some(value) = &amount_field.new_value {
                if let Some(value::Typed::String(v)) = &value.typed {
                    if let Ok(amount) = v.parse::<i64>() {
                        store.add(0, &change.id, amount);
                    }
                }
            }
        }
    }
}

// 5. 储存 15m 净交易量和总交易量
#[substreams::handlers::store]
pub fn store_15m_volume(changes: EntityChanges, store: StoreAddInt64) {
    for change in changes.entity_changes {
        if change.entity != "volume_15m" {
            continue;
        }

        if let Some(amount_field) = change.fields.iter().find(|f| f.name == "amount") {
            if let Some(value) = &amount_field.new_value {
                if let Some(value::Typed::String(v)) = &value.typed {
                    if let Ok(amount) = v.parse::<i64>() {
                        store.add(0, &change.id, amount);
                    }
                }
            }
        }
    }
}

// 5. 最后的map模块：生成统计和警报
#[substreams::handlers::map]
pub fn map_trade_stats(
    changes: EntityChanges,
    token_store: StoreGetString,
    volume_5m_store: StoreGetInt64,
    volume_15m_store: StoreGetInt64,
) -> Result<EntityChanges, substreams::errors::Error> {
    let mut tables = Tables::new();

    // 从 changes 中收集当前块的相关 token 和时间戳
    let mut current_tokens: HashMap<String, i64> = HashMap::new();

    for change in changes.entity_changes {
        if change.entity != "volume_5m" && change.entity != "volume_15m" {
            continue;
        }

        // 从实体 ID 中提取 token 地址和时间戳
        // 格式: {token_address}:{window_start}:{net/total}
        let parts: Vec<&str> = change.id.split(':').collect();
        if parts.len() != 3 {
            continue;
        }

        let token_address = parts[0].to_string();
        if let Ok(window_start) = parts[1].parse::<i64>() {
            current_tokens.insert(token_address, window_start);
        }
    }

    // 处理每个 token 的统计数据
    for (token_address, current_window_start) in current_tokens {
        // 确保 token 已经存在足够长时间（15分钟）
        if let Some(creation_time_str) = token_store.get_at(0, &format!("token:{}", token_address))
        {
            if let Ok(creation_ts) = creation_time_str.parse::<i64>() {
                // 检查 token 是否已经存在超过 15 分钟
                if current_window_start - creation_ts < 900 {
                    continue;
                }

                // 获取 5 分钟净交易量
                let net_key_5m = format!("{}:{}:net", token_address, current_window_start);
                let net_volume_5m = match volume_5m_store.get_at(0, &net_key_5m) {
                    Some(amount) => amount,
                    None => 0,
                };

                log::debug!(
                    "5m net volume: {}sol",
                    (net_volume_5m as f64) / 1000000000.0
                );

                // 获取 15 分钟总交易量
                let mut total_volume_15m = 0i64;
                let window_15m_start =
                    current_window_start - (current_window_start - creation_ts) % 900;
                let total_key_15m = format!("{}:{}:total", token_address, window_15m_start);
                if let Some(amount) = volume_15m_store.get_at(0, &total_key_15m) {
                    total_volume_15m = amount;
                }

                log::debug!(
                    "15m total volume: {}sol",
                    (total_volume_15m as f64) / 1000000000.0
                );

                // 检查是否满足信号条件
                if net_volume_5m > 3000000000 && total_volume_15m > 0 {
                    let ratio = (net_volume_5m as f64 / total_volume_15m as f64).abs();
                    if ratio >= 0.9 {
                        log::debug!(
                            "Found signal for token: {}, 5m net volume: {}, 15m total volume: {}",
                            token_address,
                            net_volume_5m,
                            total_volume_15m,
                        );

                        tables
                            .create_row(
                                "TradingSignal",
                                format!("{}:{}", token_address, current_window_start),
                            )
                            .set("token_address", token_address)
                            .set("net_volume_5m", net_volume_5m)
                            .set("total_volume_15m", total_volume_15m)
                            .set("timestamp", current_window_start);
                    }
                }
            }
        }
    }

    Ok(tables.to_entity_changes())
}

fn process_trade(
    tables: &mut Tables,
    trade: Trade,
    timestamp: i64,
    token_store: &StoreGetString,
) -> Result<(), substreams::errors::Error> {
    // 获取token创建时间戳
    if let Some(creation_time_str) =
        token_store.get_at(0, &format!("token:{}", trade.token_address))
    {
        if let Ok(creation_ts) = creation_time_str.parse::<i64>() {
            // 计算5分钟窗口
            let window_5m = 300; // 5分钟 = 300秒
            let elapsed_5m = timestamp - creation_ts;
            let n_5m = elapsed_5m / window_5m;
            let window_start_5m = creation_ts + n_5m * window_5m;

            // 计算15分钟窗口
            let window_15m = 900; // 15分钟 = 900秒
            let elapsed_15m = timestamp - creation_ts;
            let n_15m = elapsed_15m / window_15m;
            let window_start_15m = creation_ts + n_15m * window_15m;

            // 计算交易金额
            let net_amount = trade.amount;
            let total_amount = trade.amount.abs();

            // 5分钟窗口的 keys
            let net_key_5m = format!("{}:{}:net", trade.token_address, window_start_5m);
            let total_key_5m = format!("{}:{}:total", trade.token_address, window_start_5m);

            // 15分钟窗口的 keys
            let net_key_15m = format!("{}:{}:net", trade.token_address, window_start_15m);
            let total_key_15m = format!("{}:{}:total", trade.token_address, window_start_15m);

            // 创建实体变更
            tables
                .create_row("volume_5m", net_key_5m)
                .set("amount", net_amount.to_string());
            tables
                .create_row("volume_5m", total_key_5m)
                .set("amount", total_amount.to_string());
            tables
                .create_row("volume_15m", net_key_15m)
                .set("amount", net_amount.to_string());
            tables
                .create_row("volume_15m", total_key_15m)
                .set("amount", total_amount.to_string());
        }
    }
    Ok(())
}

fn process_instruction_for_trades(
    instruction: &CompiledInstruction,
    message: &Message,
    meta: &TransactionStatusMeta,
    token_store: &StoreGetString,
    timestamp: i64,
    transaction: &Transaction,
) -> Result<Option<Trade>, substreams::errors::Error> {
    let program_idx = instruction.program_id_index as usize;
    if program_idx >= message.account_keys.len() {
        return Ok(None);
    }

    let program_id = bs58::encode(&message.account_keys[program_idx]).into_string();

    match program_id.as_str() {
        PUMP_PROGRAM => {
            let data = &instruction.data;
            if data.len() < 8 {
                return Ok(None);
            }

            let method_id = hex::encode(&data[..8]);
            let is_buy = method_id == BUY_DISCRIMINATOR;
            let is_sell = method_id == SELL_DISCRIMINATOR;

            if !is_buy && !is_sell {
                return Ok(None);
            }

            // 获取 mint account (index 2)
            let mint = if let Some(account_index) = instruction.accounts.get(2) {
                let account_index: usize = *account_index as usize;
                if account_index < message.account_keys.len() {
                    bs58::encode(&message.account_keys[account_index]).into_string()
                } else {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            };

            if !is_tracked_token(token_store, &mint) {
                return Ok(None);
            }

            // 获取 bonding curve account (index 1)
            if let Some(bonding_curve_idx) = instruction.accounts.get(3) {
                let bonding_curve_idx = *bonding_curve_idx as usize;
                let bonding_curve = if bonding_curve_idx < message.account_keys.len() {
                    bs58::encode(&message.account_keys[bonding_curve_idx]).into_string()
                } else {
                    return Ok(None);
                };

                // log::debug!("Bonding curve: {}", bonding_curve);

                // if let Some(signature) = transaction.signatures.first() {
                //     let signature_str = bs58::encode(signature).into_string();
                //     log::debug!("Transaction signature: {}", signature_str);
                // }
                let account_table_index = if let Some(msg) = &transaction.message {
                    msg.account_keys.iter().position(|key| {
                        let key_str = bs58::encode(key).into_string();
                        // log::debug!("Account: {}", key_str);
                        key_str == bonding_curve
                    })
                } else {
                    return Ok(None);
                };
                // log::debug!("bonding curve index: {}",account_table_index.unwrap());
                // log::debug!(
                //     "Pre token balances: {:#?}\nPost token balances: {:#?}",
                //     meta.pre_balances,
                //     meta.post_balances
                // );

                if let Some(sol_change) = get_sol_change(
                    &meta.pre_balances,
                    &meta.post_balances,
                    account_table_index.unwrap(),
                ) {
                    if sol_change.abs() < 100000 {
                        return Ok(None);
                    }
                    // log::debug!("SOL change for user: {}", sol_change);

                    return Ok(Some(Trade {
                        token_address: mint,
                        amount: sol_change, // 根据需要调整单位
                        trade_type: if is_buy { "buy" } else { "sell" }.to_string(),
                        timestamp,
                    }));
                }
            }
        }
        RAYDIUM_PROGRAM => {
            let data = &instruction.data;
            if data.is_empty() || data[0] != 0x09 {
                return Ok(None);
            }

            let source_change =
                get_token_change_with_mint(&meta.pre_token_balances, &meta.post_token_balances, 13);

            let dest_change =
                get_token_change_with_mint(&meta.pre_token_balances, &meta.post_token_balances, 14);

            if let (Some((source_mint, source_amount)), Some((dest_mint, dest_amount))) =
                (source_change, dest_change)
            {
                // if let Some(signature) = transaction.signatures.first() {
                //     let signature_str = bs58::encode(signature).into_string();
                //     log::debug!("Transaction signature: {}", signature_str);
                // }
                // log::debug!("source mint: {}, amount: {}", source_mint, source_amount);
                // log::debug!("dest mint: {}, amount: {}", dest_mint, dest_amount);
                if source_mint == WSOL_MINT
                    && is_tracked_token(token_store, &dest_mint)
                    && source_amount.abs() >= 100000
                {
                    log::debug!("ray buy");
                    return Ok(Some(Trade {
                        token_address: dest_mint,
                        amount: source_amount.abs(),
                        trade_type: "buy".to_string(),
                        timestamp,
                    }));
                } else if dest_mint == WSOL_MINT
                    && is_tracked_token(token_store, &source_mint)
                    && dest_amount.abs() >= 100000
                {
                    log::debug!("ray sell");
                    return Ok(Some(Trade {
                        token_address: source_mint,
                        amount: -(dest_amount.abs()),
                        trade_type: "sell".to_string(),
                        timestamp,
                    }));
                }
            }
        }
        _ => {}
    }

    Ok(None)
}

fn get_token_change_with_mint(
    pre_balances: &[substreams_solana::pb::sf::solana::r#type::v1::TokenBalance],
    post_balances: &[substreams_solana::pb::sf::solana::r#type::v1::TokenBalance],
    account_index: usize,
) -> Option<(String, i64)> {
    let pre = pre_balances
        .iter()
        .find(|b| b.account_index as usize == account_index)?;
    let post = post_balances
        .iter()
        .find(|b| b.account_index as usize == account_index)?;

    let pre_amount = pre.ui_token_amount.as_ref()?.amount.parse::<i64>().ok()?;
    let post_amount = post.ui_token_amount.as_ref()?.amount.parse::<i64>().ok()?;

    if pre_amount != post_amount {
        Some((pre.mint.clone(), post_amount - pre_amount))
    } else {
        None
    }
}

fn get_sol_change(
    pre_balances: &[u64],
    post_balances: &[u64],
    account_index: usize,
) -> Option<i64> {
    if account_index >= pre_balances.len() || account_index >= post_balances.len() {
        return None;
    }

    let pre_amount = pre_balances[account_index];
    let post_amount = post_balances[account_index];

    if pre_amount != post_amount {
        // 转换为 i64 以支持正负值
        Some((post_amount as i64) - (pre_amount as i64))
    } else {
        None
    }
}

fn is_tracked_token(store: &StoreGetString, token: &str) -> bool {
    store.get_at(0, &format!("token:{}", token)).is_some()
}

fn parse_filters_from_params(
    params: String,
) -> Result<InstructionFilters, substreams::errors::Error> {
    if params.is_empty() {
        return Ok(InstructionFilters {
            program_ids: vec![],
        });
    }

    // Split the string by "||" to get individual program filters
    let program_ids: Vec<String> = params
        .split("||")
        .filter_map(|part| {
            let part = part.trim();
            if part.starts_with("program:") {
                // Extract the program ID part after "program:"
                Some(part[8..].to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(InstructionFilters { program_ids })
}

fn apply_filter(
    instruction: &CompiledInstruction,
    filters: &InstructionFilters,
    account_keys: &Vec<&Vec<u8>>,
) -> bool {
    // If no program_ids filters are specified, accept all instructions
    if filters.program_ids.is_empty() {
        return true;
    }

    // Get the program account key for this instruction
    let program_account_key = match account_keys.get(instruction.program_id_index as usize) {
        Some(key) => key,
        None => return false,
    };

    // Convert program account key to base58 string
    let program_account_key_str = bs58::encode(program_account_key).into_string();

    // Check if the instruction's program_id matches any of our filtered program_ids
    filters
        .program_ids
        .iter()
        .any(|program_id| program_id == &program_account_key_str)
}
