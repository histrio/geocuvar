use anyhow::{Context, Result};
use log::{debug, info};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_xml_rs::from_str;
use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use tokio::fs;

fn deserialize_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Deserialize};
    let s: String = Deserialize::deserialize(deserializer)?;
    s.parse::<f64>().map_err(de::Error::custom)
}

#[derive(Default, Debug, Deserialize, Serialize)]
struct BoundingBox {
    #[serde(rename = "min_lon", deserialize_with = "deserialize_f64")]
    min_lon: f64,
    #[serde(rename = "min_lat", deserialize_with = "deserialize_f64")]
    min_lat: f64,
    #[serde(rename = "max_lon", deserialize_with = "deserialize_f64")]
    max_lon: f64,
    #[serde(rename = "max_lat", deserialize_with = "deserialize_f64")]
    max_lat: f64,
}

#[derive(Debug, Deserialize, Serialize)]
struct Changeset {
    id: u64,
    created_at: String,
    open: bool,
    comments_count: u32,
    changes_count: u32,
    #[serde(flatten)]
    bbox: Option<BoundingBox>,
    uid: u64,
    user: String,
    tag: Option<Vec<Tag>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Content {
    PublishDate: String,
    id: u64,
    user: String,
    comment: String,
    created_by: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Tag {
    k: String,
    v: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
struct Osm {
    version: String,
    generator: String,
    copyright: String,
    attribution: String,
    license: String,
    changeset: Vec<Changeset>,
}

impl BoundingBox {
    fn new(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Self {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
        }
    }

    fn intersects(&self, other: &BoundingBox) -> bool {
        self.min_lon < other.max_lon
            && self.max_lon > other.min_lon
            && self.min_lat < other.max_lat
            && self.max_lat > other.min_lat
    }
}

const REPLICATION_SERVER: &str = "https://planet.openstreetmap.org/replication";

async fn get_remote_latest_changeset_id() -> Result<i32> {
    let url = format!("{}/minute/state.txt", REPLICATION_SERVER);
    let response = reqwest::get(url).await?;
    let body = response.text().await?;

    let prefix = "sequenceNumber=";
    body.lines()
        .find(|line| line.starts_with(prefix))
        .and_then(|line| line.split('=').nth(1))
        .ok_or_else(|| anyhow::anyhow!("Changeset ID not found in the response"))
        .and_then(|s| s.parse::<i32>().map_err(|e| anyhow::anyhow!("Failed to parse ID: {}", e)))
}

async fn get_home_folder() -> Result<PathBuf> {
    let project_dir = std::env::current_dir().context("Failed to get current directory")?;
    let dir_path = project_dir.join("site/");
    fs::create_dir_all(&dir_path).await.context("Failed to create directory")?;
    Ok(dir_path)
}

async fn get_local_latest_changeset_id() -> Result<i32> {
    let dir_path = get_home_folder().await?;
    let file_path = dir_path.join("sequence");

    debug!("Local state file located at: {:?}", file_path);
    fs::create_dir_all(&dir_path).await.context("Failed to create directory")?;

    let file_content = fs::read_to_string(&file_path).await.unwrap_or_else(|_| "0".to_string());
    debug!("File content: {:?}", file_content);

    file_content
        .trim()
        .parse::<i32>()
        .context("Failed to parse local changeset ID")
}

async fn write_local_latest_changeset_id(id: i32) -> Result<()> {
    let dir_path = get_home_folder().await?;
    let file_path = dir_path.join("sequence");

    debug!("Local state file located at: {:?}", file_path);
    fs::write(&file_path, id.to_string())
        .await
        .context("Failed to write changeset ID to file")?;

    debug!("Changeset ID {} written to file", id);
    Ok(())
}

fn get_changesets(xml_data: &str) -> Result<Vec<String>> {
    let mut reader = Reader::from_str(xml_data);
    reader.trim_text(true);
    let mut unique_values = HashSet::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                for attr in e.attributes().filter_map(Result::ok) {
                    if attr.key.as_ref() == b"changeset" {
                        let value = attr
                            .unescape_value()
                            .context("Failed to decode attribute value")?
                            .to_string();
                        unique_values.insert(value);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "XML parsing error at position {}: {}",
                    reader.buffer_position(),
                    e
                ))
            }
            _ => (),
        }
    }
    Ok(unique_values.into_iter().collect())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let dir_path = get_home_folder().await?;
    let montenegro_bbox = BoundingBox::new(18.4500, 41.8500, 20.3500, 43.5500);
    let remote_id = get_remote_latest_changeset_id().await?;
    let local_id = get_local_latest_changeset_id().await?;
    debug!("Remote ID: {}, Local ID: {}", remote_id, local_id);

    for id in local_id..remote_id {
        debug!("Update sequence: {}", id);
        let id_padded = format!("{:09}", id);
        let url = format!(
            "{}/minute/{}/{}/{}.osc.gz",
            REPLICATION_SERVER,
            &id_padded[0..3],
            &id_padded[3..6],
            &id_padded[6..9],
        );
        debug!("Update URL: {}", url);
        let response = reqwest::get(&url).await?;
        let body = response.bytes().await?;
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut xml_data = String::new();
        decoder.read_to_string(&mut xml_data)?;

        let changesets_list = get_changesets(&xml_data)?;
        debug!("Changesets: {:?}", changesets_list);
        let changesets_csv = changesets_list.join(",");
        let changesets_url = format!(
            "https://api.openstreetmap.org/api/0.6/changesets?changesets={}",
            changesets_csv
        );

        let response = reqwest::get(&changesets_url).await?;
        let body = response.text().await?;
        debug!("XML: {}", body);

        let osm_data: Osm = from_str(&body)?;
        debug!("Changesets: {:?}", osm_data);

        for changeset in osm_data.changeset {
            debug!("Changeset: {:?}", changeset);
            let content = Content {
                PublishDate: changeset.created_at.clone(),
                id: changeset.id,
                user: changeset.user,
                comment: changeset
                    .tag
                    .as_ref()
                    .and_then(|tags| {
                        tags.iter()
                            .find(|tag| tag.k == "comment")
                            .map(|tag| tag.v.clone())
                    })
                    .unwrap_or_else(|| "".to_string()),
                created_by: changeset
                    .tag
                    .as_ref()
                    .and_then(|tags| {
                        tags.iter()
                            .find(|tag| tag.k == "created_by")
                            .map(|tag| tag.v.clone())
                    })
                    .unwrap_or_else(|| "".to_string()),
            };
            let yaml_string = serde_yaml::to_string(&content)?;

            if let Some(bbox) = changeset.bbox {
                if bbox.intersects(&montenegro_bbox) {
                    info!("Changeset intersects Montenegro: {:?}", content);
                    let file_path = dir_path
                        .join("content")
                        .join("changesets")
                        .join(format!("{}.md", changeset.id));
                    let file_content = format!("{}\n---\n", yaml_string);

                    fs::write(&file_path, file_content)
                        .await
                        .context("Failed to write changeset file")?;
                }
            } else {
                debug!("Changeset has no bbox");
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        write_local_latest_changeset_id(id).await?;
    }

    Ok(())
}
