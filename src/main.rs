// Reimplemention of a project named "whodidit". It's  OpenStreetMap Changeset Analyzer which mosly used for generating rss
// with recent changes in an area (bbox). That script is supposed to get all recent changes from
// Opensterrmap replcation API and generate files for each of it. Rss will be generated separately.

use anyhow::Context;
use anyhow::{anyhow, Result};
use log::{debug, info};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest;
use std::path::PathBuf;
use tokio::fs;
use std::io::Read;
use std::collections::HashSet;
use quick_xml::{name::QName};
use serde_xml_rs::from_str;

use serde::{Deserialize, Serialize};

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
    //closed_at: String,
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
        BoundingBox {
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
    for line in body.lines() {
        if line.starts_with(prefix) {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() == 2 {
                return parts[1]
                    .parse::<i32>()
                    .map_err(|e| anyhow!("Failed to parse ID: {}", e));
            }
        }
    }
    Err(anyhow!("Changeset ID not found in the response"))
}

async fn get_home_folder() -> Result<PathBuf> {
    // get the projects's folder
    let project_dir = std::env::current_dir().context("Failed to get current directory")?;
    let dir_path = project_dir.join("site/");
    fs::create_dir_all(&dir_path)
        .await
        .context("Failed to create .mapchanges directory")?;
    Ok(dir_path)
}

async fn get_local_latest_changeset_id() -> Result<i32> {

    let dir_path = get_home_folder().await?;
    let file_path = dir_path.join("sequence");

    debug!("Local state file will be located at: {:?}", file_path);

    // Create the `.mapchanges` directory if it doesn't exist
    fs::create_dir_all(&dir_path)
        .await
        .context("Failed to create .mapchanges directory")?;

    // Read the contents of the `sequence` file asynchronously
    let file_content = fs::read_to_string(&file_path)
        .await
        .unwrap_or_else(|_| "0".to_string());
    debug!("File content: {:?}", file_content);

    // Attempt to parse the file content into an i32 (after trim), defaulting to 0 if parsing fails
    let changeset_id = file_content
        .trim()
        .parse::<i32>()
        .context("Failed to parse local changeset ID")?;

    info!("Latest local changeset ID is {}", changeset_id);
    Ok(changeset_id)
}

async fn write_local_latest_changeset_id(id: i32) -> Result<()> {
    let dir_path = get_home_folder().await?;
    let file_path = dir_path.join("sequence");

    debug!("Local state file will be located at: {:?}", file_path);

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
                    if attr.key == QName(b"changeset") {
                        let value_result = attr.unescape_value()
                            .map_err(|e| anyhow::Error::new(e))
                            .context("Failed to decode attribute value");

                        if let Ok(value) = value_result {
                            let value_str = value.to_string();
                            unique_values.insert(value_str);
                        }
                    }
                }
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("XML parsing error at position {}: {}", reader.buffer_position(), e)),
            _ => (),
        }
    }
    Ok(unique_values.into_iter().collect())
}



#[tokio::main]
async fn main() {
    env_logger::init();
    let dir_path = get_home_folder().await.unwrap();
    let montenegro_bbox = BoundingBox::new(18.4500, 41.8500, 20.3500, 43.5500);
    let remote_id = get_remote_latest_changeset_id().await.unwrap();
    let local_id = get_local_latest_changeset_id().await.unwrap();
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
        let response = reqwest::get(&url).await.unwrap();
        let body = response.bytes().await.unwrap();
        // Let's read from gzip file body
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut xml_data = String::new();
        decoder.read_to_string(&mut xml_data).unwrap();
        let changesets_list = get_changesets(&xml_data).unwrap();
        debug!("Changesets: {:?}", changesets_list);
        // comma separated list of changesets
        let changesets_csv = changesets_list.join(",");
        let changesets_url = format!(
            "https://api.openstreetmap.org/api/0.6/changesets?changesets={}",
            changesets_csv
        );
        let response = reqwest::get(&changesets_url).await.unwrap();
        let body = response.text().await.unwrap();
        debug!("XML: {}", body);
        let osm_data: Osm = from_str(&body).unwrap();
        debug!("Changesets: {:?}", osm_data);

        for changeset in osm_data.changeset {
            debug!("Changeset: {:?}", changeset);
            // tag is optional
            let content = Content {
                PublishDate: changeset.created_at.clone(),
                id: changeset.id,
                user: changeset.user,
                comment: changeset.tag.as_ref().and_then(|tags| {
                    tags.iter()
                        .find(|tag| tag.k == "comment")
                        .map(|tag| tag.v.clone())
                }).unwrap_or_else(|| "".to_string()),
                created_by: changeset.tag.as_ref().and_then(|tags| {
                    tags.iter()
                        .find(|tag| tag.k == "created_by")
                        .map(|tag| tag.v.clone())
                }).unwrap_or_else(|| "".to_string()),
            };
            let yaml_string = serde_yaml::to_string(&content).unwrap();
            debug!("Content: {}", yaml_string);
            match changeset.bbox {
                Some(bbox) => {
                    if bbox.intersects(&montenegro_bbox) {
                        info!("Changeset intersects Montenegro: {:?}", content);
                        let file_path = dir_path.join("content").join("changesets").join(format!("{}.md", changeset.id));
                        let file_content = format!(
                            "{}\n\
                            ---\n",
                            yaml_string
                        ); 
                        fs::write(&file_path, file_content).await.context("Failed to write changeset file").unwrap();
                    }
                },
                None => {
                    debug!("Changeset has no bbox");
                },

            }
        }
        // Respect Openstreetmap API rate limits
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        write_local_latest_changeset_id(id).await.unwrap();

    }
}
