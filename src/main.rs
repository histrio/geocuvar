use anyhow::{Context, Result};
use log::{debug, info};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_xml_rs::from_str;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;
use tokio::fs;
use xml::reader::{EventReader, XmlEvent};

use geo::algorithm::contains::Contains;
use geo::Polygon;

use chrono::{DateTime, Utc};

// Declare turbopass query as string alias
type TurbopassQuery = String;

type ChangesetID = String;

fn deserialize_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Deserialize};
    let s: String = Deserialize::deserialize(deserializer)?;
    s.parse::<f64>().map_err(de::Error::custom)
}

// ------------------------------------------------------------------------------------------------

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
    #[serde(rename = "Title")]
    title: String,
    #[serde(rename = "PublishDate")]
    publish_date: String,
    id: u64,
    user: String,
    comment: String,
    created_by: String,
    tags: Vec<String>,
    bbox: BoundingBox,
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

// ------------------------------------------------------------------------------------------------
// Overpass API

#[derive(Debug, Deserialize)]
pub struct Overpass {
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "generator")]
    pub generator: String,
    #[serde(rename = "note")]
    pub note: Option<String>,
    #[serde(rename = "meta")]
    pub meta: Meta,
    #[serde(rename = "way")]
    pub ways: Vec<Way>, // Handle multiple <relation> elements
}

#[derive(Debug, Deserialize)]
pub struct Way {
    #[serde(rename = "id")]
    pub id: i64,
    #[serde(rename = "bounds")]
    pub bounds: Option<Bounds>,
    #[serde(rename = "nd")]
    pub nodes: Vec<Node>, // Multiple nodes
}

#[derive(Debug, Deserialize)]
pub struct OverpassTag {
    #[serde(rename = "k")]
    pub k: String,
    #[serde(rename = "v")]
    pub v: String,
}

#[derive(Debug, Deserialize)]
pub struct Node {
    #[serde(rename = "ref")]
    pub ref_id: i64,
    lat: f64,
    lon: f64,
}

#[derive(Debug, Deserialize)]
pub struct Bounds {
    minlat: f64,
    minlon: f64,
    maxlat: f64,
    maxlon: f64,
}

#[derive(Debug, Deserialize)]
pub struct Meta {
    #[serde(rename = "osm_base")]
    pub osm_base: String,
}

// ------------------------------------------------------------------------------------------------

trait Boundaries {
    fn intersects(&self, other: &BoundingBox) -> bool;
}

struct BoundingPolygon {
    polygon: Polygon,
}

impl BoundingPolygon {
    async fn new(query: TurbopassQuery) -> Result<Self> {
        info!("Calculating bounding polygon for query: {}", query);
        let turbopass_query = format!("[out:xml]; {}; out geom;", query);
        let turbopass_query_url = format!(
            "https://overpass-api.de/api/interpreter?data={}",
            turbopass_query
        );
        let response = reqwest::get(&turbopass_query_url).await?;
        let xml_data = response.text().await?;
        let overpass: Overpass = from_str(&xml_data)?;

        let mut polygon_points = vec![];
        // pop first way as current way
        let mut current_way = &overpass.ways[0];
        let mut reverse = false;
        let mut processed_ways = vec![];
        while !processed_ways.contains(&current_way.id) {
            processed_ways.push(current_way.id);
            if reverse {
                for node in current_way.nodes.iter().rev() {
                    polygon_points.push((node.lon, node.lat));
                }
            } else {
                for node in current_way.nodes.iter() {
                    polygon_points.push((node.lon, node.lat));
                }
            }

            let last_node = if reverse {
                current_way.nodes.first().unwrap()
            } else {
                current_way.nodes.last().unwrap()
            };

            // find next way
            if let Some(next_way) = overpass.ways.iter().find(|way| {
                way.nodes.first().unwrap().ref_id == last_node.ref_id && way.id != current_way.id
            }) {
                reverse = false;
                current_way = next_way;
            } else if let Some(next_way) = overpass.ways.iter().find(|way| {
                way.nodes.last().unwrap().ref_id == last_node.ref_id && way.id != current_way.id
            }) {
                reverse = true;
                current_way = next_way;
            } else {
                let ways_count = overpass.ways.len();
                if ways_count == processed_ways.len() {
                    break;
                }
                panic!("no next way found");
            }
        }
        // Dump polygon points
        // use tokio::io::AsyncWriteExt;
        //fs::write("polygon_points.txt", format!("{:?}", polygon_points)).await?;

        let polygon = Polygon::new(polygon_points.into(), vec![]);
        info!("Polygon points count: {}", polygon.exterior().0.len());

        // use geo::SimplifyVw;
        //let simplified_polygon = polygon.simplify_vw(&0.000001);
        //info!("Polygon points count after simplification: {}", simplified_polygon.exterior().0.len());
        Ok(Self { polygon })
    }
}

impl Boundaries for BoundingPolygon {
    fn intersects(&self, other: &BoundingBox) -> bool {
        // in some cases it cound be a point when coords are the same
        if other.min_lon == other.max_lon && other.min_lat == other.max_lat {
            return self
                .polygon
                .contains(&geo::Point::new(other.min_lon, other.min_lat));
        }
        // counterclockwise order for the exterior ring
        let other_polygon_points = vec![
            (other.min_lon, other.min_lat), // Bottom-left
            (other.max_lon, other.min_lat), // Bottom-right
            (other.max_lon, other.max_lat), // Top-right
            (other.min_lon, other.max_lat), // Top-left
            (other.min_lon, other.min_lat), // Close the loop (back to bottom-left)
        ];
        let other_polygon = Polygon::new(other_polygon_points.into(), vec![]);
        self.polygon.contains(&other_polygon)
    }
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
}

impl Boundaries for BoundingBox {
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
        .and_then(|s| {
            s.parse::<i32>()
                .map_err(|e| anyhow::anyhow!("Failed to parse ID: {}", e))
        })
}

async fn get_home_folder() -> Result<PathBuf> {
    let project_dir = std::env::current_dir().context("Failed to get current directory")?;
    let dir_path = project_dir.join("site/");
    fs::create_dir_all(&dir_path)
        .await
        .context("Failed to create directory")?;
    Ok(dir_path)
}

async fn get_local_latest_changeset_id() -> Result<i32> {
    let dir_path = get_home_folder().await?;
    let file_path = dir_path.join("sequence");

    debug!("Local state file located at: {:?}", file_path);
    fs::create_dir_all(&dir_path)
        .await
        .context("Failed to create directory")?;

    let file_content = fs::read_to_string(&file_path)
        .await
        .unwrap_or_else(|_| "0".to_string());
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

fn get_changesets_list(xml_data: &str) -> Result<Vec<ChangesetID>> {
    let parser = EventReader::from_str(xml_data);
    let mut unique_values = HashSet::new();

    for e in parser {
        match e {
            Ok(XmlEvent::StartElement { attributes, .. }) => {
                for attr in attributes {
                    if attr.name.local_name == "changeset" {
                        let value = attr.value.clone();
                        unique_values.insert(value);
                    }
                }
            }
            Ok(XmlEvent::EndDocument) => break,
            Err(e) => {
                return Err(anyhow::anyhow!("XML parsing error: {}", e));
            }
            _ => (),
        }
    }

    Ok(unique_values.into_iter().collect())
}

async fn get_changeset_xml_data(id: i32) -> Result<String> {
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
    Ok(xml_data.to_string())
}

async fn get_osm_data(chunk: &[String]) -> Result<Osm> {
    let changesets_csv = chunk.join(",");
    let changesets_url = format!(
        "https://api.openstreetmap.org/api/0.6/changesets?changesets={}",
        changesets_csv
    );

    let response = reqwest::get(&changesets_url).await?;
    let body = response.text().await?;
    debug!("XML: {}", body);

    let osm_data: Osm = from_str(&body)?;
    Ok(osm_data)
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let dir_path = get_home_folder().await?;

    let bounding_boxes: Vec<(&str, Box<dyn Boundaries>)> = vec![
        (
            "Montenegro",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Crna Gora / Црна Гора']['boundary'='administrative']; way(r)"
                        .to_string(),
                )
                .await?,
            ),
        ),
        (
            "Budva",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Budva']['boundary'='administrative']; way(r)".to_string(),
                )
                .await?,
            ),
        ),
        (
            "Budva+",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Opština Budva']['boundary'='administrative']; way(r)"
                        .to_string(),
                )
                .await?,
            ),
        ),
        (
            "Kotor",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Kotor']['boundary'='administrative']; way(r)".to_string(),
                )
                .await?,
            ),
        ),
        (
            "Kotor+",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Opština Kotor']['boundary'='administrative']; way(r)"
                        .to_string(),
                )
                .await?,
            ),
        ),
        (
            "Cetinje",
            Box::new(
                BoundingPolygon::new("way['name'='Cetinje']['place'='town']".to_string()).await?,
            ),
        ),
        (
            "Cetinje+",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Prijestolnica Cetinje']['boundary'='administrative']; way(r)"
                        .to_string(),
                )
                .await?,
            ),
        ),
        (
            "Tivat",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Tivat']['boundary'='administrative']; way(r)".to_string(),
                )
                .await?,
            ),
        ),
        /* (
            "Tivat+",
            //BoundingBox::new(18.6645, 42.4014, 18.7050, 42.4350),
            Box::new(BoundingPolygon::new("relation['name'='Opština Tivat']['boundary'='administrative']; way(r)".to_string()).await?),
        ), */
        (
            "Bar",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Bar']['boundary'='administrative']; way(r)".to_string(),
                )
                .await?,
            ),
        ),
        /* (
            "Bar+",
            Box::new(BoundingPolygon::new("relation['name'='Opština Bar']['boundary'='administrative']; way(r)".to_string()).await?),
        ), */
        (
            "Podgorica",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Podgorica']['boundary'='administrative']; way(r)".to_string(),
                )
                .await?,
            ),
        ),
        (
            "Podgorica+",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Glavni grad Podgorica']['boundary'='administrative']; way(r)"
                        .to_string(),
                )
                .await?,
            ),
        ),
        (
            "Nikšić",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Nikšić']['boundary'='administrative']; way(r)".to_string(),
                )
                .await?,
            ),
        ),
        (
            "Nikšić+",
            Box::new(
                BoundingPolygon::new(
                    "relation['name'='Opština Nikšić']['boundary'='administrative']; way(r)"
                        .to_string(),
                )
                .await?,
            ),
        ),
    ];

    let remote_id = get_remote_latest_changeset_id().await?;
    let local_id = get_local_latest_changeset_id().await?;
    debug!("Remote ID: {}, Local ID: {}", remote_id, local_id);

    for id in local_id..remote_id {
        info!(
            "Processing changeset {} of {}",
            id - local_id + 1,
            remote_id - local_id + 1
        );

        let xml_data = get_changeset_xml_data(id).await?;
        let changesets_list = get_changesets_list(&xml_data)?;
        debug!("Changesets: {:?}", changesets_list);

        // Get changesets as chunks size of 20 and iter over it
        // to avoid 414 Request-URI Too Large

        let chunks = changesets_list.chunks(20);
        for _changesets_list in chunks {
            let osm_data = get_osm_data(_changesets_list).await?;
            debug!("Changesets: {:?}", osm_data);

            for changeset in osm_data.changeset {
                debug!("Changeset: {:?}", changeset);

                let mut tags: Vec<String> = Vec::new();
                if let Some(ref bbox) = changeset.bbox {
                    for (tag, region_bbox) in &bounding_boxes {
                        if region_bbox.intersects(&bbox) {
                            info!("Changeset intersects {}: {:?}", tag, changeset.id);
                            tags.push(tag.to_string());
                        }
                    }
                }

                if !tags.is_empty() {
                    let comment = changeset
                        .tag
                        .as_ref()
                        .and_then(|tags| {
                            tags.iter()
                                .find(|tag| tag.k == "comment")
                                .map(|tag| tag.v.clone())
                        })
                        .unwrap_or_else(|| "".to_string());

                    let bbox = changeset.bbox.unwrap_or_default();

                    let created_by = changeset
                        .tag
                        .as_ref()
                        .and_then(|tags| {
                            tags.iter()
                                .find(|tag| tag.k == "created_by")
                                .map(|tag| tag.v.clone())
                        })
                        .unwrap_or_else(|| "".to_string());

                    let content = Content {
                        title: format!("Changeset #{}", changeset.id),
                        publish_date: changeset.created_at.clone(),
                        id: changeset.id,
                        user: changeset.user,
                        comment,
                        created_by,
                        tags,
                        bbox: bbox,
                    };

                    let yaml_string = serde_yaml::to_string(&content)?;
                    let file_path = dir_path
                        .join("content")
                        .join("changesets")
                        .join(format!("{}.md", changeset.id));
                    let file_content = format!("---\n{}\n---\n", yaml_string);

                    fs::write(&file_path, file_content)
                        .await
                        .context("Failed to write changeset file")?;
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        write_local_latest_changeset_id(id).await?;
    }

    // Get files older than 1 year and remove them
    let one_year_ago = chrono::Utc::now() - chrono::Duration::days(365);
    let changesets_dir = dir_path.join("content").join("changesets");
    info!(
        "Removing files older than one year in: {:?}",
        changesets_dir
    );

    // iter over dir *.md files and remove those older than one year
    let mut iter_files = fs::read_dir(changesets_dir).await?;
    while let Some(entry_result) = iter_files.next_entry().await? {
        if entry_result.path().extension().and_then(OsStr::to_str) == Some("md") {
            let metadata = entry_result.metadata().await?;
            if let Ok(modified) = metadata.modified() {
                let modified_time: DateTime<Utc> = modified.into();
                if modified_time < one_year_ago {
                    info!("Removing file: {:?}", entry_result.path());
                    fs::remove_file(entry_result.path()).await?;
                }
            }
        }
    }

    Ok(())
}
