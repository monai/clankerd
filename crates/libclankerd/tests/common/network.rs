use clankerd_network_ipc::{Request, Response, Settings};
use libclankerd::net::NetworkService;
use std::{collections::BTreeMap, io, sync::Mutex};

#[derive(Default)]
pub struct Service {
    pub entries: Mutex<BTreeMap<String, (String, Settings)>>,
    pub history: Mutex<Vec<Settings>>,
    pub failure: Mutex<Option<String>>,
    pub wrong_prefix: bool,
    pub serial: Mutex<u64>,
}
impl Service {
    pub fn last(&self) -> Settings {
        self.history.lock().unwrap().last().unwrap().clone()
    }
    pub fn count(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}
impl NetworkService for Service {
    fn request(&self, request: &Request) -> io::Result<Response> {
        if let Some(error) = self.failure.lock().unwrap().as_ref() {
            return Err(io::Error::other(error.clone()));
        }
        let mut entries = self.entries.lock().unwrap();
        let mut response = Response::default();
        match request {
            Request::List {} => response.interfaces = vec!["en0".into(), "en1".into()],
            Request::Create { key, settings } => {
                let mut serial = self.serial.lock().unwrap();
                *serial += 1;
                let generation = serial.to_string();
                response.generation = Some(generation.clone());
                response.prefix = if self.wrong_prefix {
                    Some("fd42:bad::".into())
                } else {
                    settings.nat66_prefix.clone()
                };
                response.connected = true;
                self.history.lock().unwrap().push(settings.clone());
                entries.insert(key.clone(), (generation, settings.clone()));
            }
            Request::Stop { key, generation } => {
                if entries.get(key).is_some_and(|(g, _)| g == generation) {
                    entries.remove(key);
                }
            }
            Request::Status { key, generation } => {
                response.connected = entries.get(key).is_some_and(|(g, _)| g == generation)
            }
        }
        Ok(response)
    }
}
