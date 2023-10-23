# databento-rs

[![build](https://github.com/databento/databento-rs/actions/workflows/build.yaml/badge.svg)](https://github.com/databento/dbn/actions/workflows/build.yaml)
[![license](https://img.shields.io/github/license/databento/databento-rs?color=blue)](./LICENSE)
[![Current Crates.io Version](https://img.shields.io/crates/v/databento.svg)](https://crates.io/crates/databento)
[![Slack](https://img.shields.io/badge/join_Slack-community-darkblue.svg?logo=slack)](https://join.slack.com/t/databento-hq/shared_invite/zt-24oqyrub9-MellISM2cdpQ7s_7wcXosw)

The official Rust client library for [Databento](https://databento.com).
The clients support fast and safe streaming of both real-time and historical market data
through similar interfaces.

## Installation

To add the crate to an existing project, run the following command:
```sh
cargo add databento
```

## Usage

### Live

Real-time and intraday replay is provided through the Live clients.
Here is a simple program that fetches the next ES mini futures trade:

```rust
use std::error::Error;

use databento::{
    dbn::{Dataset, SType, Schema, TradeMsg},
    live::{Subscription, SymbolMap},
    LiveClient,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut client = LiveClient::builder()
        .key_from_env()?
        .dataset(Dataset::GlbxMdp3)
        .build()
        .await?;
    client
        .subscribe(
            &Subscription::builder()
                .symbols("ES.FUT")
                .schema(Schema::Trades)
                .stype_in(SType::Parent)
                .build(),
        )
        .await
        .unwrap();
    client.start().await?;

    let mut symbol_map = SymbolMap::new();
    // Get the next trade
    loop {
        let rec = client.next_record().await?.unwrap();
        if let Some(trade) = rec.get::<TradeMsg>() {
            let symbol = &symbol_map[trade.hd.instrument_id];
            println!("Received trade for {symbol}: {trade:?}",);
            break;
        }
        symbol_map.on_record(rec)?;
    }
    Ok(())
}
```
To run this program, set the `DATABENTO_API_KEY` environment variable with an actual API key.

### Historical

Here is a simple program that fetches 10 minutes worth of historical trades for the entire CME Globex market:
```rust
use std::error::Error;

use databento::{
    dbn::{Schema, TradeMsg},
    historical::timeseries::GetRangeParams,
    HistoricalClient, Symbols,
};
use time::macros::datetime;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut client = HistoricalClient::builder().key_from_env()?.build()?;
    let mut decoder = client
        .timeseries()
        .get_range(
            &GetRangeParams::builder()
                .dataset("GLBX.MDP3")
                .date_time_range((
                    datetime!(2022-06-10 14:30 UTC),
                    datetime!(2022-06-10 14:40 UTC),
                ))
                .symbols(Symbols::All)
                .schema(Schema::Trades)
                .build(),
        )
        .await?;
    while let Some(trade) = decoder.decode_record::<TradeMsg>().await? {
        println!("{trade:?}");
    }
    Ok(())
}
```

To run this program, set the `DATABENTO_API_KEY` environment variable with an actual API key.

## Documentation

You can find more detailed examples and the full API documentation on the [Databento docs site](https://docs.databento.com/getting-started?historical=rust&live=rust).

## License

Distributed under the [Apache 2.0 License](https://www.apache.org/licenses/LICENSE-2.0.html).
