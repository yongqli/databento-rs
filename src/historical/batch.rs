//! The historical batch download API.

use core::fmt;
use std::{
    collections::HashMap,
    fmt::Write,
    num::NonZeroU64,
    path::{Path, PathBuf},
    str::FromStr,
};

use dbn::enums::{Compression, Encoding, SType, Schema};
use futures::StreamExt;
use log::info;
use reqwest::RequestBuilder;
use serde::{de, Deserialize, Deserializer};
use time::OffsetDateTime;
use tokio::io::BufWriter;
use typed_builder::TypedBuilder;

use crate::{Error, Symbols};

use super::DateTimeRange;

/// A client for the batch group of Historical API endpoints.
pub struct BatchClient<'a> {
    pub(crate) inner: &'a mut super::Client,
}

impl BatchClient<'_> {
    /// Submits a new batch job and returns a description and identifiers for the job.
    ///
    /// # Errors
    /// This function returns an error when it fails to communicate with the Databento API
    /// or the API indicates there's an issue with the request.
    pub async fn submit_job(&mut self, params: &SubmitJobParams) -> crate::Result<BatchJob> {
        let mut form = vec![
            ("dataset", params.dataset.to_string()),
            ("schema", params.schema.to_string()),
            ("compression", params.compression.to_string()),
            ("encoding", Encoding::Dbn.to_string()),
            ("split_duration", params.split_duration.to_string()),
            (
                "packaging",
                params
                    .packaging
                    .map_or_else(|| "none".to_owned(), |p| p.to_string()),
            ),
            ("delivery", params.delivery.to_string()),
            ("stype_in", params.stype_in.to_string()),
            ("stype_out", params.stype_out.to_string()),
            ("symbols", params.symbols.to_api_string()),
        ];
        params.date_time_range.add_to_form(&mut form);
        if let Some(split_size) = params.split_size {
            form.push(("split_size", split_size.to_string()));
        }
        if let Some(limit) = params.limit {
            form.push(("limit", limit.to_string()));
        }
        let builder = self.post("submit_job")?.form(&form);
        Ok(builder.send().await?.error_for_status()?.json().await?)
    }

    /// Lists previous batch jobs with filtering by `params`.
    ///
    /// # Errors
    /// This function returns an error when it fails to communicate with the Databento API
    /// or the API indicates there's an issue with the request.
    pub async fn list_jobs(&mut self, params: &ListJobsParams) -> crate::Result<Vec<BatchJob>> {
        let mut builder = self.get("list_jobs")?;
        if let Some(ref states) = params.states {
            let states_str = states.iter().fold(String::new(), |mut acc, s| {
                if acc.is_empty() {
                    s.as_str().to_owned()
                } else {
                    write!(acc, ",{}", s.as_str()).unwrap();
                    acc
                }
            });
            builder = builder.query(&[("states", states_str)]);
        }
        if let Some(ref since) = params.since {
            builder = builder.query(&[("since", &since.unix_timestamp_nanos().to_string())]);
        }
        Ok(builder.send().await?.error_for_status()?.json().await?)
    }

    /// Lists all files associated with the batch job with ID `job_id`.
    ///
    /// # Errors
    /// This function returns an error when it fails to communicate with the Databento API
    /// or the API indicates there's an issue with the request.
    pub async fn list_files(&mut self, job_id: &str) -> crate::Result<Vec<BatchFileDesc>> {
        Ok(self
            .get("list_files")?
            .query(&[("job_id", job_id)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Downloads the file specified in `params` or all files associated with the job ID.
    ///
    /// # Errors
    /// This function returns an error when it fails to communicate with the Databento API
    /// or the API indicates there's an issue with the request. It will also return an
    /// error if it encounters an issue downloading a file.
    pub async fn download(&mut self, params: &DownloadParams) -> crate::Result<Vec<PathBuf>> {
        let job_dir = params.output_dir.join(&params.job_id);
        if job_dir.exists() {
            if !job_dir.is_dir() {
                return Err(Error::bad_arg(
                    "output_dir",
                    "exists but is not a directory",
                ));
            }
        } else {
            tokio::fs::create_dir_all(&job_dir).await?;
        }
        let job_files = self.list_files(&params.job_id).await?;
        if let Some(filename_to_download) = params.filename_to_download.as_ref() {
            let Some(file_desc) = job_files
                .iter()
                .find(|file| file.filename == *filename_to_download)
            else {
                return Err(Error::bad_arg(
                    "filename_to_download",
                    "not found for batch job",
                ));
            };
            let output_path = job_dir.join(filename_to_download);
            let https_url = file_desc
                .urls
                .get("https")
                .ok_or_else(|| Error::internal("Missing https URL for batch file"))?;
            self.download_file(https_url, &output_path).await?;
            Ok(vec![output_path])
        } else {
            let mut paths = Vec::new();
            for file_desc in job_files.iter() {
                let output_path = params
                    .output_dir
                    .join(&params.job_id)
                    .join(&file_desc.filename);
                let https_url = file_desc
                    .urls
                    .get("https")
                    .ok_or_else(|| Error::internal("Missing https URL for batch file"))?;
                self.download_file(https_url, &output_path).await?;
                paths.push(output_path);
            }
            Ok(paths)
        }
    }

    async fn download_file(&mut self, url: &str, path: impl AsRef<Path>) -> crate::Result<()> {
        let url = reqwest::Url::parse(url)
            .map_err(|e| Error::internal(format!("Unable to parse URL: {e:?}")))?;
        let mut stream = self
            .inner
            .get_with_path(url.path())?
            .send()
            .await?
            .error_for_status()?
            .bytes_stream();
        info!("Saving {url} to {}", path.as_ref().display());
        let mut output = BufWriter::new(
            tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .await?,
        );
        while let Some(chunk) = stream.next().await {
            tokio::io::copy(&mut chunk?.as_ref(), &mut output).await?;
        }
        Ok(())
    }

    const PATH_PREFIX: &str = "batch";

    fn get(&mut self, slug: &str) -> crate::Result<RequestBuilder> {
        self.inner.get(&format!("{}.{slug}", Self::PATH_PREFIX))
    }

    fn post(&mut self, slug: &str) -> crate::Result<RequestBuilder> {
        self.inner.post(&format!("{}.{slug}", Self::PATH_PREFIX))
    }
}

/// The duration of time at which batch files will be split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplitDuration {
    /// One file per day.
    #[default]
    Day,
    /// One file per week. A week starts on Sunday UTC.
    Week,
    /// One file per month.
    Month,
}

/// How the batch job will be packaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packaging {
    /// ZIP compressed.
    Zip,
    /// Tarball.
    Tar,
}

/// How the batch job will be delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delivery {
    /// Via download from the Databento portal.
    #[default]
    Download,
    /// Via Amazon S3.
    S3,
    /// Via disk.
    Disk,
}

/// The state of a batch job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    /// The job has been received (the initial state).
    Received,
    /// The job has been queued for processing.
    Queued,
    /// The job has begun processing.
    Processing,
    /// The job has finished processing and is ready for delivery.
    Done,
    /// The job is no longer available.
    Expired,
}

/// The parameters for [`BatchClient::submit_job()`]. Use [`SubmitJobParams::builder()`] to
/// get a builder type with all the preset defaults.
#[derive(Debug, Clone, TypedBuilder)]
pub struct SubmitJobParams {
    /// The dataset code.
    #[builder(setter(transform = |dt: impl ToString| dt.to_string()))]
    pub dataset: String,
    /// The symbols to filter for.
    #[builder(setter(into))]
    pub symbols: Symbols,
    /// The data record schema.
    pub schema: Schema,
    /// The request time range.
    #[builder(setter(into))]
    pub date_time_range: DateTimeRange,
    /// The data compression mode. Defaults to [`ZStd`](dbn::enums::Compression::ZStd).
    #[builder(default = Compression::ZStd)]
    pub compression: Compression,
    /// The maximum time duration before batched data is split into multiple files.
    /// Defaults to [`Day`](SplitDuration::Day).
    #[builder(default)]
    pub split_duration: SplitDuration,
    /// The optional maximum size (in bytes) of each batched data file before being split.
    /// Defaults to `None`.
    #[builder(default, setter(strip_option))]
    pub split_size: Option<NonZeroU64>,
    /// The optional archive type to package all batched data files in. Defaults to `None`.
    #[builder(default, setter(strip_option))]
    pub packaging: Option<Packaging>,
    /// The delivery mechanism for the batched data files once processed. Defaults to
    /// [`Download`](Delivery::Download).
    #[builder(default)]
    pub delivery: Delivery,
    /// The symbology type of the input `symbols`. Defaults to
    /// [`RawSymbol`](dbn::enums::SType::RawSymbol).
    #[builder(default = SType::RawSymbol)]
    pub stype_in: SType,
    /// The symbology type of the output `symbols`. Defaults to
    /// [`InstrumentId`](dbn::enums::SType::InstrumentId).
    #[builder(default = SType::InstrumentId)]
    pub stype_out: SType,
    /// The optional maximum number of records to return. Defaults to no limit.
    #[builder(default)]
    pub limit: Option<NonZeroU64>,
}

/// The description of a submitted batch job.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchJob {
    /// The unique job ID.
    pub id: String,
    /// The user ID of the user who submitted the job.
    pub user_id: Option<String>,
    /// The bill ID (for internal use).
    pub bill_id: Option<String>,
    /// The cost of the job in US dollars. Will be `None` until the job is processed.
    pub cost_usd: Option<f64>,
    /// The dataset code.
    pub dataset: String,
    /// The list of symbols specified in the request.
    pub symbols: Symbols,
    /// The symbology type of the input `symbols`.
    pub stype_in: SType,
    /// The symbology type of the output `symbols`.
    pub stype_out: SType,
    /// The data record schema.
    pub schema: Schema,
    /// The start of the request time range (inclusive).
    #[serde(deserialize_with = "deserialize_date_time")]
    pub start: OffsetDateTime,
    /// The end of the request time range (exclusive).
    #[serde(deserialize_with = "deserialize_date_time")]
    pub end: OffsetDateTime,
    /// The maximum number of records to return.
    pub limit: Option<NonZeroU64>,
    /// The data encoding.
    pub encoding: Encoding,
    /// The data compression mode.
    #[serde(deserialize_with = "deserialize_compression")]
    pub compression: Compression,
    /// The maximum time interval for an individual file before splitting into multiple
    /// files.
    pub split_duration: SplitDuration,
    /// The maximum size for an individual file before splitting into multiple files.
    pub split_size: Option<NonZeroU64>,
    /// The packaging method of the batch data.
    pub packaging: Option<Packaging>,
    /// The delivery mechanism of the batch data.
    pub delivery: Delivery,
    /// The number of data records (`None` until the job is processed).
    pub record_count: Option<u64>,
    /// The size of the raw binary data used to process the batch job (used for billing purposes).
    pub billed_size: Option<u64>,
    /// The total size of the result of the batch job after splitting and compression.
    pub actual_size: Option<u64>,
    /// The total size of the result of the batch job after any packaging (including metadata).
    pub package_size: Option<u64>,
    /// The current status of the batch job.
    pub state: JobState,
    /// The timestamp of when Databento received the batch job.
    #[serde(deserialize_with = "deserialize_date_time")]
    pub ts_received: OffsetDateTime,
    /// The timestamp of when the batch job was queued.
    #[serde(deserialize_with = "deserialize_opt_date_time")]
    pub ts_queued: Option<OffsetDateTime>,
    /// The timestamp of when the batch job began processing.
    #[serde(deserialize_with = "deserialize_opt_date_time")]
    pub ts_process_start: Option<OffsetDateTime>,
    /// The timestamp of when the batch job finished processing.
    #[serde(deserialize_with = "deserialize_opt_date_time")]
    pub ts_process_done: Option<OffsetDateTime>,
    /// The timestamp of when the batch job will expire from the Download center.
    #[serde(deserialize_with = "deserialize_opt_date_time")]
    pub ts_expiration: Option<OffsetDateTime>,
}

/// The parameters for [`BatchClient::list_jobs()`]. Use [`ListJobsParams::builder()`] to
/// get a builder type with all the preset defaults.
#[derive(Debug, Clone, Default, TypedBuilder)]
pub struct ListJobsParams {
    /// The optional filter for job states.
    #[builder(default, setter(strip_option))]
    pub states: Option<Vec<JobState>>,
    /// The optional filter for timestamp submitted (will not include jobs prior to
    /// this time).
    #[builder(default, setter(strip_option))]
    pub since: Option<OffsetDateTime>,
}

/// The file details for a batch job.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchFileDesc {
    /// The file name.
    pub filename: String,
    /// The size of the file in bytes.
    pub size: u64,
    /// The SHA256 hash of the file.
    pub hash: String,
    /// A map of download protocol to URL.
    pub urls: HashMap<String, String>,
}

/// The parameters for [`BatchClient::download()`]. Use [`DownloadParams::builder()`] to
/// get a builder type with all the preset defaults.
#[derive(Debug, Clone, TypedBuilder)]
pub struct DownloadParams {
    /// The directory to download the file(s) to.
    pub output_dir: PathBuf,
    /// The batch job identifier.
    pub job_id: String,
    /// `None` means all files associated with the job will be downloaded.
    #[builder(default, setter(strip_option))]
    pub filename_to_download: Option<String>,
}

const DATE_TIME_FORMAT: &[time::format_description::FormatItem<'static>] =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second][optional [.[subsecond digits:6]]][offset_hour padding:zero]:[offset_minute padding:zero]");

fn deserialize_date_time<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<time::OffsetDateTime, D::Error> {
    let dt_str = String::deserialize(deserializer)?;
    time::OffsetDateTime::parse(&dt_str, DATE_TIME_FORMAT).map_err(serde::de::Error::custom)
}

fn deserialize_opt_date_time<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<time::OffsetDateTime>, D::Error> {
    pub const DATE_TIME_FORMAT: &[time::format_description::FormatItem<'static>] =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6][offset_hour padding:zero]:[offset_minute padding:zero]");
    if let Some(dt_str) = Option::<String>::deserialize(deserializer)? {
        time::OffsetDateTime::parse(&dt_str, DATE_TIME_FORMAT)
            .map(Some)
            .map_err(serde::de::Error::custom)
    } else {
        Ok(None)
    }
}

impl SplitDuration {
    /// Converts the enum to its `str` representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            SplitDuration::Day => "day",
            SplitDuration::Week => "week",
            SplitDuration::Month => "month",
        }
    }
}

impl fmt::Display for SplitDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SplitDuration {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "day" => Ok(SplitDuration::Day),
            "week" => Ok(SplitDuration::Week),
            "month" => Ok(SplitDuration::Month),
            _ => Err(crate::Error::bad_arg(
                "s",
                format!(
                    "{s} does not correspond with any {} variant",
                    std::any::type_name::<Self>()
                ),
            )),
        }
    }
}

impl<'de> Deserialize<'de> for SplitDuration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let str = String::deserialize(deserializer)?;
        FromStr::from_str(&str).map_err(de::Error::custom)
    }
}

impl Packaging {
    /// Converts the enum to its `str` representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Packaging::Zip => "zip",
            Packaging::Tar => "tar",
        }
    }
}

impl fmt::Display for Packaging {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Packaging {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "zip" => Ok(Packaging::Zip),
            "tar" => Ok(Packaging::Tar),
            _ => Err(crate::Error::bad_arg(
                "s",
                format!(
                    "{s} does not correspond with any {} variant",
                    std::any::type_name::<Self>()
                ),
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Packaging {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let str = String::deserialize(deserializer)?;
        FromStr::from_str(&str).map_err(de::Error::custom)
    }
}

impl Delivery {
    /// Converts the enum to its `str` representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Delivery::Download => "download",
            Delivery::S3 => "s3",
            Delivery::Disk => "disk",
        }
    }
}

impl fmt::Display for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Delivery {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "download" => Ok(Delivery::Download),
            "s3" => Ok(Delivery::S3),
            "disk" => Ok(Delivery::Disk),
            _ => Err(crate::Error::bad_arg(
                "s",
                format!(
                    "{s} does not correspond with any {} variant",
                    std::any::type_name::<Self>()
                ),
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Delivery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let str = String::deserialize(deserializer)?;
        FromStr::from_str(&str).map_err(de::Error::custom)
    }
}

impl JobState {
    /// Converts the enum to its `str` representation.
    pub const fn as_str(&self) -> &'static str {
        match self {
            JobState::Received => "received",
            JobState::Queued => "queued",
            JobState::Processing => "processing",
            JobState::Done => "done",
            JobState::Expired => "expired",
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobState {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "received" => Ok(JobState::Received),
            "queued" => Ok(JobState::Queued),
            "processing" => Ok(JobState::Processing),
            "done" => Ok(JobState::Done),
            "expired" => Ok(JobState::Expired),
            _ => Err(crate::Error::bad_arg(
                "s",
                format!(
                    "{s} does not correspond with any {} variant",
                    std::any::type_name::<Self>()
                ),
            )),
        }
    }
}

impl<'de> Deserialize<'de> for JobState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let str = String::deserialize(deserializer)?;
        FromStr::from_str(&str).map_err(de::Error::custom)
    }
}

// Handles Compression::None being serialized as null in JSON
fn deserialize_compression<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Compression, D::Error> {
    let opt = Option::<Compression>::deserialize(deserializer)?;
    Ok(opt.unwrap_or(Compression::None))
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::json;
    use time::macros::datetime;
    use wiremock::{
        matchers::{basic_auth, method, path, query_param_is_missing},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;
    use crate::{
        body_contains,
        historical::{HistoricalGateway, API_VERSION},
        HistoricalClient,
    };

    const API_KEY: &str = "test-batch";

    #[tokio::test]
    async fn test_submit_job() -> crate::Result<()> {
        const START: time::OffsetDateTime = datetime!(2023 - 06 - 14 00:00 UTC);
        const END: time::OffsetDateTime = datetime!(2023 - 06 - 17 00:00 UTC);
        const SCHEMA: Schema = Schema::Trades;

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(basic_auth(API_KEY, ""))
            .and(path(format!("/v{API_VERSION}/batch.submit_job")))
            .and(body_contains("dataset", "XNAS.ITCH"))
            .and(body_contains("schema", "trades"))
            .and(body_contains("symbols", "TSLA"))
            .and(body_contains(
                "start",
                START.unix_timestamp_nanos().to_string(),
            ))
            .and(body_contains("end", END.unix_timestamp_nanos().to_string()))
            // // default
            .and(body_contains("stype_in", "raw_symbol"))
            .and(body_contains("stype_out", "instrument_id"))
            .respond_with(ResponseTemplate::new(StatusCode::OK).set_body_json(json!({
                "id": "123",
                "user_id": "test_user",
                "bill_id": "345",
                "cost_usd": 10.50,
                "dataset": "XNAS.ITCH",
                "symbols": ["TSLA"],
                "stype_in": "raw_symbol",
                "stype_out": "instrument_id",
                "schema": SCHEMA.as_str(),
                "start": "2023-06-14 00:00:00.000000+00:00",
                "end": "2023-06-17 00:00:00.000000+00:00",
                "limit": null,
                "encoding": "dbn",
                "compression": "zstd",
                "split_duration": "day",
                "split_size": null,
                "packaging": null,
                "delivery": "download",
                "state": "queued",
                 "ts_received": "2023-07-19 23:00:04.095538+00:00",
                 "ts_queued": null,
                 "ts_process_start": null,
                 "ts_process_done": null,
                 "ts_expiration": null
            })))
            .mount(&mock_server)
            .await;
        let mut target = HistoricalClient::with_url(
            mock_server.uri(),
            API_KEY.to_owned(),
            HistoricalGateway::Bo1,
        )?;
        let job_desc = target
            .batch()
            .submit_job(
                &SubmitJobParams::builder()
                    .dataset(dbn::datasets::XNAS_ITCH)
                    .schema(SCHEMA)
                    .symbols("TSLA")
                    .date_time_range((START, END))
                    .build(),
            )
            .await?;
        assert_eq!(job_desc.dataset, dbn::datasets::XNAS_ITCH);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_jobs() -> crate::Result<()> {
        const SCHEMA: Schema = Schema::Trades;

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(basic_auth(API_KEY, ""))
            .and(path(format!("/v{API_VERSION}/batch.list_jobs")))
            .and(query_param_is_missing("states"))
            .and(query_param_is_missing("since"))
            .respond_with(ResponseTemplate::new(StatusCode::OK).set_body_json(json!([{
                "id": "123",
                "user_id": "test_user",
                "bill_id": "345",
                "cost_usd": 10.50,
                "dataset": "XNAS.ITCH",
                "symbols": "TSLA",
                "stype_in": "raw_symbol",
                "stype_out": "instrument_id",
                "schema": SCHEMA.as_str(),
                "start": "2023-06-14 00:00:00+00:00",
                "end": "2023-06-17 00:00:00+00:00",
                "limit": null,
                "encoding": "dbn",
                "compression": "zstd",
                "split_duration": "day",
                "split_size": null,
                "packaging": null,
                "delivery": "download",
                "state": "processing",
                 "ts_received": "2023-07-19 23:00:04.095538+00:00",
                 "ts_queued": "2023-07-19 23:00:08.095538+00:00",
                 "ts_process_start": "2023-07-19 23:01:04.000000+00:00",
                 "ts_process_done": null,
                 "ts_expiration": null
            }])))
            .mount(&mock_server)
            .await;
        let mut target = HistoricalClient::with_url(
            mock_server.uri(),
            API_KEY.to_owned(),
            HistoricalGateway::Bo1,
        )?;
        let job_descs = target.batch().list_jobs(&ListJobsParams::default()).await?;
        assert_eq!(job_descs.len(), 1);
        let job_desc = &job_descs[0];
        assert_eq!(
            job_desc.ts_queued.unwrap(),
            datetime!(2023-07-19 23:00:08.095538 UTC)
        );
        assert_eq!(
            job_desc.ts_process_start.unwrap(),
            datetime!(2023-07-19 23:01:04 UTC)
        );
        Ok(())
    }

    #[test]
    fn test_deserialize_compression() {
        #[derive(serde::Deserialize)]
        struct Test {
            #[serde(deserialize_with = "deserialize_compression")]
            compression: Compression,
        }

        const JSON: &str =
            r#"[{"compression":null}, {"compression":"none"}, {"compression":"zstd"}]"#;
        let res: Vec<Test> = serde_json::from_str(JSON).unwrap();
        assert_eq!(
            res.into_iter().map(|t| t.compression).collect::<Vec<_>>(),
            vec![Compression::None, Compression::None, Compression::ZStd]
        );
    }
}
