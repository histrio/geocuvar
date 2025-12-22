use anyhow::{Context, Result};
use log::{debug, info, warn};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_xml_rs::from_str;
use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use tokio::fs;
use xml::reader::{EventReader, XmlEvent};

use geo::algorithm::contains::Contains;
use geo::Polygon;

use chrono::{DateTime, Utc};

use geo::{coord};

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
#[allow(dead_code)]
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

#[derive(Debug, Deserialize, Serialize)]
struct BoundingPolygon {
    polygons: Vec<Polygon>,
}

async fn get_overpass(query: &TurbopassQuery) -> Result<Overpass> {
    let turbopass_query = format!("[out:xml]; {}; out geom;", query);

    info!(
        "Calculating bounding polygon for query: '{}'",
        turbopass_query
    );
    let turbopass_query_url = format!(
        "https://overpass-api.de/api/interpreter?data={}",
        turbopass_query
    );
    let response = reqwest::get(&turbopass_query_url).await?;
    let xml_data = response.text().await?;
    let overpass: Overpass = from_str(&xml_data)?;
    Ok(overpass)
}

impl BoundingPolygon {
    async fn new(query: TurbopassQuery) -> Result<Self> {
        let overpass = get_overpass(&query).await?;
        let ways = &overpass.ways;

        let mut polygons = Vec::new();
        let mut visited = HashSet::new();

        // Iterate over all ways; each unvisited way can start a new polygon
        for start_way in ways {
            if visited.contains(&start_way.id) {
                continue;
            }

            let mut polygon_points = Vec::new();
            let mut current_way = start_way;
            let mut reverse = false;
            visited.insert(current_way.id);

            let first_node_id = current_way.nodes.first().unwrap().ref_id;

            loop {
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
                if last_node.ref_id == first_node_id {
                    break;
                }

                if let Some(next_way) = ways.iter().find(|way| {
                    !visited.contains(&way.id)
                        && way.nodes.first().map_or(false, |n| n.ref_id == last_node.ref_id)
                }) {
                    reverse = false;
                    visited.insert(next_way.id);
                    current_way = next_way;
                } else if let Some(next_way) = ways.iter().find(|way| {
                    !visited.contains(&way.id)
                        && way.nodes.last().map_or(false, |n| n.ref_id == last_node.ref_id)
                }) {
                    reverse = true;
                    visited.insert(next_way.id);
                    current_way = next_way;
                } else {
                    warn!("No next way found for way {}", current_way.id);
                    break;
                }
            }

            if let Some(first_pt) = polygon_points.first().cloned() {
                let last_pt = polygon_points.last().cloned().unwrap();
                if first_pt != last_pt {
                    polygon_points.push(first_pt); // close the ring
                }
            }

            let polygon = Polygon::new(
                polygon_points
                    .into_iter()
                    .map(|(lon, lat)| coord! { x: lon, y: lat })
                    .collect::<Vec<_>>()
                    .into(),
                vec![], // no interior holes
            );
            info!("Polygon exterior has {} points", polygon.exterior().0.len());
            polygons.push(polygon);
        }

        info!("Detected {} polygons", polygons.len());

        Ok(Self { polygons })
    }
}

impl Boundaries for BoundingPolygon {
    fn intersects(&self, other: &BoundingBox) -> bool {
        // in some cases it count be a point when coords are the same
        // let polygon = &self.polygons[0];
        for polygon in &self.polygons {
            if other.min_lon == other.max_lon && other.min_lat == other.max_lat {
                return polygon.contains(&geo::Point::new(other.min_lon, other.min_lat));
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
            if polygon.contains(&other_polygon) {
                return true;
            }
        }
        false   
    }
}

impl BoundingBox {
    #[allow(dead_code)]
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

type RegionName = String;
type BoundingBoxes = Vec<(RegionName, BoundingPolygon)>;

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

async fn dump_data(file_path: &PathBuf, content: &Content) -> Result<()> {
    let yaml_string = serde_yaml::to_string(&content)?;
    let file_content = format!("---\n{}\n---\n", yaml_string);
    fs::write(&file_path, file_content)
        .await
        .context("Failed to write changeset file")?;
    Ok(())
}

async fn get_tag_value(changeset: &Changeset, tag_name: &str) -> String {
    match &changeset.tag {
        Some(tags) => match tags.iter().find(|tag| tag.k == tag_name) {
            Some(tag) => tag.v.clone(),
            None => String::new(),
        },
        None => String::new(),
    }
}

async fn get_bounding_boxes() -> Result<BoundingBoxes> {
    let dir_path = get_home_folder().await?;

    let cached_boundaries = dir_path.join("boundaries.yaml");

    if cached_boundaries.is_file() {
        let month_ago = chrono::Utc::now() - chrono::Duration::days(30);
        let metadata = fs::metadata(&cached_boundaries).await?;
        let created = metadata.created()?;
        let created_time: DateTime<Utc> = created.into();
        if created_time > month_ago {
            info!("Cache hit in {:?}", &cached_boundaries);
            let yaml_string = fs::read_to_string(&cached_boundaries).await?;
            let bounding_boxes: BoundingBoxes = serde_yaml::from_str(&yaml_string)?;
            return Ok(bounding_boxes);
        } else {
            info!("Cache miss: too old {:?}", created_time);
        }
    } else {
        info!("Cache miss: no such file {:?}", &cached_boundaries);
    }

    let bounding_boxes: BoundingBoxes = vec![
        (
            "Montenegro".to_string(),
            BoundingPolygon::new(
                "relation['name'='Crna Gora / Црна Гора']['boundary'='administrative']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Budva".to_string(),
            BoundingPolygon::new(
                "relation['name'='Budva']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Budva+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Budva']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Kotor".to_string(),
            BoundingPolygon::new(
                "relation['name'='Kotor']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Kotor+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Kotor']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Cetinje".to_string(),
            BoundingPolygon::new("way['name'='Cetinje']['place'='town']".to_string()).await?,
        ),
        (
            "Cetinje+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Prijestolnica Cetinje']['boundary'='administrative']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Tivat".to_string(),
            BoundingPolygon::new(
                "relation['name'='Tivat']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Tivat+".to_string(),
            BoundingPolygon::new("relation['name'='Opština Tivat']['boundary'='administrative']; way(r)".to_string()).await?,
        ),
        (
            "Bar".to_string(),
            BoundingPolygon::new(
                "relation['name'='Bar']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Bar+".to_string(),
            BoundingPolygon::new("relation['name'='Opština Bar']['boundary'='administrative']; way(r)".to_string()).await?,
        ),
        (
            "Podgorica".to_string(),
            BoundingPolygon::new(
                "relation['name'='Podgorica']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Podgorica+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Glavni grad Podgorica']['boundary'='administrative']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Nikšić".to_string(),
            BoundingPolygon::new(
                "relation['name'='Nikšić']['boundary'='administrative']; way(r)".to_string(),
            )
            .await?,
        ),
        (
            "Nikšić+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Nikšić']['boundary'='administrative']; way(r)"
                    .to_string(),
            )
            .await?,
        ),

        (
            "Andrijevica+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Andrijevica']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Herceg Novi+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Herceg Novi']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Plav+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Plav']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Plav".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Gusinje']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Berane+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Berane']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Rožaje+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Rožaje']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Petnjica+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Petnjica']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Bijelo Polje+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Bijelo Polje']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Pljevlja+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Pljevlja']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Plužine+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Plužine']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Žabljak+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Žabljak']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Šavnik+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Šavnik']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Mojkovac+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Mojkovac']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Kolašin+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Kolašin']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Danilovgrad+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Danilovgrad']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Tuzi+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Tuzi']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Zeta+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Zeta']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
        (
            "Ulcinj+".to_string(),
            BoundingPolygon::new(
                "relation['name'='Opština Ulcinj - Komuna e Ulqinit']['boundary'='administrative']['admin_level'='6']; way(r)"
                    .to_string(),
            )
            .await?,
        ),
    ];

    let yaml_string = serde_yaml::to_string(&bounding_boxes)?;
    fs::write(&cached_boundaries, yaml_string)
        .await
        .context(format!("Can't crate cache file: {:?}", &cached_boundaries))?;

    Ok(bounding_boxes)
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let dir_path = get_home_folder().await?;

    let remote_id = get_remote_latest_changeset_id().await?;
    let local_id = get_local_latest_changeset_id().await?;
    debug!("Remote ID: {}, Local ID: {}", remote_id, local_id);

    let changesets_dir = dir_path.join("content").join("changesets");

    let bounding_boxes = get_bounding_boxes().await?;

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
                    let comment = get_tag_value(&changeset, "comment").await;
                    let created_by = get_tag_value(&changeset, "created_by").await;
                    let bbox = changeset.bbox.unwrap_or_default();
                    let title = format!("Changeset #{}", changeset.id);
                    let publish_date = changeset.created_at.clone();
                    let user = changeset.user;
                    let id = changeset.id;

                    let content = Content {
                        id,
                        user,
                        title,
                        publish_date,
                        comment,
                        created_by,
                        tags,
                        bbox,
                    };

                    let file_path = changesets_dir.join(format!("{}.md", changeset.id));
                    dump_data(&file_path, &content).await?;
                    debug!("Changeset {} saved as {:?}", changeset.id, file_path);
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        write_local_latest_changeset_id(id).await?;
    }

    info!("Done");
    Ok(())
}
