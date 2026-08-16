use std::io::{IsTerminal, Read};

use anyhow::{Result, bail};
use kide_core::{QueryPayload, QueryResponse, SymbolId};

pub(crate) fn target_from_argument_or_stdin(target: Option<String>) -> Result<String> {
    let targets = targets_from_argument_or_stdin(target)?;
    match targets.as_slice() {
        [target] => Ok(target.clone()),
        _ => bail!(
            "this command requires exactly one target, but the pipe contains {}",
            targets.len()
        ),
    }
}

pub(crate) fn targets_from_argument_or_stdin(target: Option<String>) -> Result<Vec<String>> {
    match target {
        Some(target) => Ok(vec![target]),
        None => {
            if std::io::stdin().is_terminal() {
                bail!(
                    "a target is required, or pipe one `kide symbols`/`kide definition` response to stdin"
                );
            }
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            Ok(input
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(target_from_pipe_text)
                .collect::<Result<Vec<_>>>()?)
        }
    }
}

pub(crate) fn target_from_pipe_text(input: &str) -> Result<String> {
    if let Ok(record) = serde_json::from_str::<kide_core::SelectorRecord>(input) {
        return Ok(record.symbol.id.as_str().to_owned());
    }
    target_from_pipe_response(serde_json::from_str(input)?)
}

fn target_from_pipe_response(response: QueryResponse) -> Result<String> {
    match response
        .result
        .ok_or_else(|| anyhow::anyhow!("--stdin response contains no result"))?
    {
        QueryPayload::Definition { symbol } => Ok(symbol.id.as_str().to_owned()),
        QueryPayload::Symbols { symbols } => {
            target_from_symbols(symbols.into_iter().map(|symbol| symbol.id).collect())
        }
        _ => bail!("--stdin accepts only `kide symbols` or `kide definition` output"),
    }
}

pub(crate) fn target_from_symbols(symbols: Vec<SymbolId>) -> Result<String> {
    match symbols.as_slice() {
        [symbol] => Ok(symbol.as_str().to_owned()),
        _ => bail!(
            "--stdin needs exactly one symbol, but the upstream query returned {}",
            symbols.len()
        ),
    }
}
