use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone)]
struct Part {
    id: String,
    guid: String,
    links: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct Runtime {
    groups: Vec<Vec<Part>>,
    fade: f64,
    pub(crate) signals: BTreeMap<String, f64>,
    pub(crate) opacities: BTreeMap<String, f64>,
}

impl Runtime {
    pub(crate) fn parse(document: &Value, source: &Value) -> Result<Self, String> {
        if document["Type"] != "Live2D Pose" {
            return Err("pose Type must be Live2D Pose".into());
        }
        let fade = match document.get("FadeInTime") {
            None => 0.5,
            Some(value) => value
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or("pose FadeInTime must be finite")?,
        };
        let mut parts = BTreeMap::new();
        for part in source["sources"]
            .as_array()
            .ok_or("native source list is missing")?
            .iter()
            .filter(|s| s["data"]["type"] == "Part")
        {
            let id = part["id"].as_str().filter(|id| !id.is_empty())
                .ok_or("CMO3 Part source lacks an ID")?;
            let guid = part["guid"].as_str().filter(|guid| !guid.is_empty())
                .ok_or("CMO3 Part source lacks a GUID")?;
            if parts.insert(id, guid).is_some() {
                return Err("CMO3 Part IDs are ambiguous".into());
            }
        }
        let groups = document["Groups"]
            .as_array()
            .filter(|g| !g.is_empty() && g.len() <= 64)
            .ok_or("pose Groups must contain 1 to 64 groups")?;
        let mut seen = HashSet::new();
        let mut linked = HashSet::new();
        let mut signals = BTreeMap::new();
        let mut parsed = Vec::new();
        let mut count = 0;
        for group in groups {
            let group = group
                .as_array()
                .filter(|g| !g.is_empty())
                .ok_or("pose group must be a nonempty array")?;
            let mut entries = Vec::new();
            for record in group {
                count += 1;
                if count > 1024 {
                    return Err("pose exceeds 1024 part entries".into());
                }
                let id = record["Id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("pose Id must be a nonempty string")?;
                if !seen.insert(id) {
                    return Err(format!("pose part {id:?} is duplicated"));
                }
                let guid = parts
                    .get(id)
                    .ok_or_else(|| format!("pose references unknown Part {id:?}"))?;
                let mut links = Vec::new();
                if let Some(value) = record.get("Link") {
                    for link in value.as_array().ok_or("pose Link must be an array")? {
                        count += 1;
                        if count > 1024 {
                            return Err("pose exceeds 1024 part entries".into());
                        }
                        let link = link.as_str().ok_or("pose linked Id must be a string")?;
                        let target = parts.get(link).ok_or_else(|| {
                            format!("pose references unknown linked Part {link:?}")
                        })?;
                        if !linked.insert(link) {
                            return Err(format!("pose link {link:?} has multiple owners"));
                        }
                        links.push((*target).to_owned());
                    }
                }
                signals.insert(id.to_owned(), 1.0);
                entries.push(Part {
                    id: id.to_owned(),
                    guid: (*guid).to_owned(),
                    links,
                });
            }
            parsed.push(entries);
        }
        if !seen.is_disjoint(&linked) {
            return Err("pose linked Parts must not also be selection members".into());
        }
        let mut runtime = Self {
            groups: parsed,
            fade: if fade < 0.0 { 0.5 } else { fade },
            signals,
            opacities: BTreeMap::new(),
        };
        runtime.reset();
        Ok(runtime)
    }

    pub(crate) fn reset(&mut self) {
        self.opacities.clear();
        for value in self.signals.values_mut() {
            *value = 1.0;
        }
        for group in &self.groups {
            for (index, part) in group.iter().enumerate() {
                let opacity = if index == 0 { 1.0 } else { 0.0 };
                self.signals.insert(part.id.clone(), opacity);
                self.opacities.insert(part.guid.clone(), opacity);
            }
        }
        self.copy_links();
    }

    pub(crate) fn step(&mut self, values: &BTreeMap<String, f64>, dt: f64) -> Result<(), String> {
        if !dt.is_finite() || dt < 0.0 {
            return Err("pose dt must be finite and nonnegative".into());
        }
        if values.values().any(|v| !v.is_finite()) {
            return Err("pose signals must be finite".into());
        }
        for (id, value) in values {
            if let Some(signal) = self.signals.get_mut(id) {
                *signal = *value;
            }
        }
        for group in &self.groups {
            let active = group.iter().position(|part| self.signals[&part.id] > 0.001);
            let selected = active.unwrap_or(0);
            let opacity = if active.is_none() || self.fade == 0.0 {
                1.0
            } else {
                (self.opacities[&group[selected].guid] + dt / self.fade).min(1.0)
            };
            let mut other = 1.0 - opacity;
            if (1.0 - other) * (1.0 - opacity) > 0.15 {
                other = 1.0 - 0.15 / (1.0 - opacity);
            }
            for (index, part) in group.iter().enumerate() {
                let value = if index == selected {
                    opacity
                } else {
                    self.opacities[&part.guid].min(other)
                };
                self.opacities.insert(part.guid.clone(), value);
            }
        }
        self.copy_links();
        Ok(())
    }

    fn copy_links(&mut self) {
        for group in &self.groups {
            for part in group {
                let opacity = self.opacities[&part.guid];
                for link in &part.links {
                    self.opacities.insert(link.clone(), opacity);
                }
            }
        }
    }

    /// The selection Ids of the group that holds `id`.
    pub(crate) fn group_of(&self, id: &str) -> Option<Vec<String>> {
        self.groups.iter().find(|group| group.iter().any(|part| part.id == id))
            .map(|group| group.iter().map(|part| part.id.clone()).collect())
    }

    pub(crate) fn snapshot(&self) -> Value {
        let groups: Vec<Vec<&str>> = self.groups.iter()
            .map(|group| group.iter().map(|part| part.id.as_str()).collect()).collect();
        json!({"fadeInTime":self.fade,"signals":self.signals,"partOpacities":self.opacities,"groups":groups})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_or_missing_part_identity() {
        let document = json!({"Type":"Live2D Pose","Groups":[[{"Id":"A"}]]});
        let mut source = json!({"sources":[
            {"id":"A","guid":"first","data":{"type":"Part"}},
            {"id":"A","guid":"second","data":{"type":"Part"}}]});
        assert!(Runtime::parse(&document, &source).is_err());
        source["sources"].as_array_mut().unwrap().pop();
        source["sources"][0]["guid"] = Value::Null;
        assert!(Runtime::parse(&document, &source).is_err());
    }
    #[test]
    fn rejects_links_that_are_also_selection_members() {
        let source = json!({"sources":[
            {"id":"A","guid":"a","data":{"type":"Part"}},
            {"id":"B","guid":"b","data":{"type":"Part"}}]});
        for group in [json!([{"Id":"A","Link":["A"]}]),
            json!([{"Id":"A","Link":["B"]},{"Id":"B"}]),
            json!([{"Id":"A"},{"Id":"B","Link":["A"]}])] {
            assert!(Runtime::parse(&json!({"Type":"Live2D Pose","Groups":[group]}), &source).is_err());
        }
    }
    fn runtime() -> Runtime {
        Runtime::parse(
            &json!({"Type":"Live2D Pose","Groups":[[
            {"Id":"A","Link":["Link"]},{"Id":"B"}]]}),
            &json!({"sources":[
            {"id":"A","guid":"a","data":{"type":"Part"}},
            {"id":"B","guid":"b","data":{"type":"Part"}},
            {"id":"Link","guid":"link","data":{"type":"Part"}}]}),
        )
        .unwrap()
    }
    #[test]
    fn switching_fades_caps_background_and_copies_links() {
        let mut pose = runtime();
        assert_eq!(pose.snapshot()["groups"], json!([["A", "B"]]));
        assert!(pose.snapshot()["signals"].get("Link").is_none());
        let before = pose.snapshot();
        assert!(pose.step(&BTreeMap::from([("Link".into(), 1.0)]), 0.5).is_ok());
        assert_eq!(pose.snapshot(), before);
        assert_eq!(
            pose.opacities,
            BTreeMap::from([("a".into(), 1.0), ("b".into(), 0.0), ("link".into(), 1.0)])
        );
        pose.step(
            &BTreeMap::from([("A".into(), 0.0), ("B".into(), 1.0)]),
            0.25,
        )
        .unwrap();
        assert_eq!(pose.opacities["a"], 0.7);
        assert_eq!(pose.opacities["b"], 0.5);
        assert_eq!(pose.opacities["link"], 0.7);
        pose.step(&BTreeMap::new(), 0.25).unwrap();
        assert_eq!(pose.opacities["a"], 0.0);
        assert_eq!(pose.opacities["b"], 1.0);
        pose.reset();
        assert_eq!(pose.signals["A"], 1.0);
        assert_eq!(pose.signals["B"], 0.0);
    }
    #[test]
    fn threshold_fallback_first_active_and_failed_steps_are_stable() {
        let mut pose = runtime();
        pose.step(
            &BTreeMap::from([("A".into(), 0.001), ("B".into(), 0.0)]),
            0.0,
        )
        .unwrap();
        assert_eq!(pose.opacities["a"], 1.0);
        pose.step(&BTreeMap::from([("A".into(), 1.0), ("B".into(), 1.0)]), 0.5)
            .unwrap();
        assert_eq!(pose.opacities["b"], 0.0);
        let before = pose.snapshot();
        assert!(pose.step(&BTreeMap::new(), -1.0).is_err());
        assert_eq!(pose.snapshot(), before);
    }
}
