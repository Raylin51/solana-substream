use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;
use substreams::log;
use substreams::store::{StoreGet, StoreGetString, StoreNew, StoreSet, StoreSetString};
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

#[derive(Debug, Clone, Serialize)]
struct TokenStats {
    token_address: String,
    net_volume_5m: i64,
    total_volume_15m: i64,
    latest_trade_timestamp: i64,
    latest_trade_type: String,
    latest_trade_amount: i64,
    meets_criteria: bool,
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
                                    store.set(0, &format!("token:{}", mint), &"1".to_string());
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

    for tx in &blk.transactions {
        if let (Some(transaction), Some(meta)) = (tx.transaction.as_ref(), tx.meta.as_ref()) {
            if let Some(message) = transaction.message.as_ref() {
                // 遍历所有指令处理交易
                for instruction in &message.instructions {
                    if let Some(trade) = process_instruction_for_trades(
                        instruction,
                        message,
                        meta,
                        &token_store,
                        timestamp,
                        transaction,
                    )? {
                        // 创建交易记录以供后续存储
                        tables
                            .create_row(
                                "trades",
                                format!(
                                    "{}:{}:{}",
                                    trade.token_address, trade.trade_type, timestamp
                                ),
                            )
                            .set("tokenAddress", trade.token_address)
                            .set("amount", trade.amount.to_string())
                            .set("type", trade.trade_type)
                            .set("timestamp", timestamp.to_string());
                    }
                }
            }
        }
    }

    Ok(tables.to_entity_changes())
}

// 4. 第二个store模块：存储交易数据
#[substreams::handlers::store]
pub fn store_trade_data(changes: EntityChanges, store: StoreSetString) {
    for change in changes.entity_changes {
        if change.entity == "trades" {
            let mut json_data = json!({});

            for field in change.fields {
                if let Some(value) = field.new_value {
                    if let Some(value::Typed::String(v)) = value.typed {
                        json_data[field.name] = json!(v);
                    }
                }
            }

            // 存储交易数据
            store.set(0, &change.id, &json_data.to_string());

            // 存储时间索引
            if let Some(token_address) = json_data["tokenAddress"].as_str() {
                if let Some(timestamp) = json_data["timestamp"].as_str() {
                    let index_key = format!("time_index:{}:{}", token_address, timestamp);
                    store.set(0, &index_key, &change.id);
                }
            }
        }
    }
}

// 5. 最后的map模块：生成统计和警报
#[substreams::handlers::map]
pub fn map_trade_stats(
    changes: EntityChanges,
    trade_store: StoreGetString,
) -> Result<EntityChanges, substreams::errors::Error> {
    let mut tables = Tables::new();
    let mut current_block_trades: HashMap<String, Vec<Trade>> = HashMap::new();

    // 收集当前区块的所有交易
    for change in &changes.entity_changes {
        if change.entity == "trades" {
            let mut token_address = String::new();
            let mut trade_type = String::new();
            let mut amount = 0i64;
            let mut timestamp = 0i64;

            for field in &change.fields {
                if let Some(value) = &field.new_value {
                    if let Some(value::Typed::String(v)) = &value.typed {
                        match field.name.as_str() {
                            "tokenAddress" => token_address = v.clone(),
                            "type" => trade_type = v.clone(),
                            "amount" => amount = v.parse().unwrap_or_default(),
                            "timestamp" => timestamp = v.parse().unwrap_or_default(),
                            _ => {}
                        }
                    }
                }
            }

            if !token_address.is_empty() {
                let trade = Trade {
                    token_address: token_address.clone(),
                    amount,
                    trade_type,
                    timestamp,
                };

                current_block_trades
                    .entry(token_address)
                    .or_default()
                    .push(trade);
            }
        }
    }

    // 处理每个代币的统计数据
    for (token_address, current_trades) in current_block_trades {
        let timestamp = current_trades
            .iter()
            .map(|t| t.timestamp)
            .max()
            .unwrap_or_default();

        // 获取15分钟的历史交易数据
        let mut all_trades =
            get_trades_in_window(&trade_store, &token_address, timestamp - 900, timestamp);

        // 添加当前区块的所有相关交易
        all_trades.extend(current_trades.iter().cloned());

        // 确保交易按时间排序
        all_trades.sort_by_key(|trade| trade.timestamp);

        if all_trades.is_empty() {
            // log::debug!("Token {} has no trading history, skipping", token_address);
            continue;
        }

        // 检查第一笔交易是否超过15分钟
        let first_trade = all_trades.first().unwrap(); // 这里可以安全使用 unwrap 因为已经检查过空
        if timestamp - first_trade.timestamp < 900 {
            // log::debug!(
            //     "Token {} history too short ({} seconds), skipping",
            //     token_address,
            //     timestamp - first_trade.timestamp
            // );
            continue;
        }

        // 获取最新的交易信息
        let latest_trade = current_trades.last().unwrap();

        // 计算5分钟和15分钟的交易量
        let five_min_trades: Vec<Trade> = all_trades
            .iter()
            .filter(|t| timestamp - t.timestamp <= 300)
            .cloned() // 克隆 Trade 对象
            .collect();

        let (buy_vol_5m, sell_vol_5m) = calculate_volumes(&five_min_trades);
        let net_volume_5m = buy_vol_5m - sell_vol_5m;

        let total_volume_15m: i64 = all_trades
            .iter()
            .filter(|t| timestamp - t.timestamp <= 900)
            .map(|t| t.amount.abs())
            .sum();
        log::debug!(
            "Token: {}, 5m: {}sol, 15m: {}sol",
            token_address,
            (net_volume_5m as f64) / 1000000000.0,
            (total_volume_15m as f64) / 1000000000.0,
        );

        let mut stats = TokenStats {
            token_address: token_address.clone(),
            net_volume_5m,
            total_volume_15m,
            latest_trade_timestamp: latest_trade.timestamp,
            latest_trade_type: latest_trade.trade_type.clone(),
            latest_trade_amount: latest_trade.amount,
            meets_criteria: false,
        };

        // 计算5分钟净流入量与15分钟总交易量的比值
        if net_volume_5m > 0 {
            let ratio = (net_volume_5m as f64 / total_volume_15m as f64).abs();
            stats.meets_criteria = ratio >= 0.5;
        }

        if stats.meets_criteria {
            log::debug!(
                "Found signal for token: {}, 5m net volume: {}, 15m total volume: {}",
                token_address,
                net_volume_5m,
                total_volume_15m
            );
            if let Ok(stats_json) = serde_json::to_string(&stats) {
                log::debug!("notify");
                tables
                    .create_row(
                        "trading_signals",
                        format!("{}:{}", token_address, timestamp),
                    )
                    .set("data", stats_json);
            }
        }
    }

    Ok(tables.to_entity_changes())
}

fn get_trades_in_window(
    store: &StoreGetString,
    token_address: &str,
    start_time: i64,
    end_time: i64,
) -> Vec<Trade> {
    let mut trades = Vec::new();

    // 使用时间索引获取交易
    for timestamp in start_time..=end_time {
        let index_key = format!("time_index:{}:{}", token_address, timestamp);

        if let Some(trade_id) = store.get_at(0, &index_key) {
            if let Some(trade_data) = store.get_at(0, &trade_id) {
                if let Ok(trade_json) = serde_json::from_str::<serde_json::Value>(&trade_data) {
                    if let (Some(amount), Some(trade_type), Some(ts)) = (
                        trade_json["amount"]
                            .as_str()
                            .and_then(|s| s.parse::<i64>().ok()),
                        trade_json["type"].as_str(),
                        trade_json["timestamp"]
                            .as_str()
                            .and_then(|s| s.parse::<i64>().ok()),
                    ) {
                        // log::debug!("token: {},type:{}, amount: {} sol", token_address.to_string(), trade_type.to_string(), (amount as f64) / 1_000_000_000.0);
                        trades.push(Trade {
                            token_address: token_address.to_string(),
                            amount,
                            trade_type: trade_type.to_string(),
                            timestamp: ts,
                        });
                    }
                }
            }
        }
    }

    trades.sort_by_key(|trade| trade.timestamp);
    trades
}

fn calculate_volumes(trades: &[Trade]) -> (i64, i64) {
    let mut buy_volume = 0i64;
    let mut sell_volume = 0i64;

    for trade in trades {
        match trade.trade_type.as_str() {
            "buy" => buy_volume += trade.amount,
            "sell" => sell_volume += trade.amount,
            _ => {}
        }
    }

    (buy_volume, sell_volume)
}

fn get_first_trade_time(store: &StoreGetString, token_address: &str) -> Option<i64> {
    // 先检查代币是否被跟踪
    let token_key = format!("token:{}", token_address);
    if store.get_at(0, &token_key).is_none() {
        return None;
    }

    // 直接获取最早交易时间
    let first_trade_key = format!("first_trade:{}", token_address);
    store
        .get_at(0, &first_trade_key)
        .and_then(|ts| ts.parse::<i64>().ok())
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

                if let Some(sol_change) =
                    get_sol_change(&meta.pre_balances, &meta.post_balances, account_table_index.unwrap())
                {
                    if sol_change.abs() < 100000 {
                        return  Ok(None);
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
                if source_mint == WSOL_MINT && is_tracked_token(token_store, &dest_mint) && source_amount.abs() >= 100000  {
                    log::debug!("ray buy");
                    return Ok(Some(Trade {
                        token_address: dest_mint,
                        amount: source_amount.abs(),
                        trade_type: "buy".to_string(),
                        timestamp,
                    }));
                } else if dest_mint == WSOL_MINT && is_tracked_token(token_store, &source_mint) && dest_amount.abs() >= 100000 {
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
