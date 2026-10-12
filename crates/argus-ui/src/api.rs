use std::fmt;

use gloo_net::http::{Request, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct Failure {
    pub status: u16,
    pub text: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

fn failure(text: impl ToString) -> Failure {
    Failure { status: 0, text: text.to_string() }
}

async fn checked(response: Result<Response, gloo_net::Error>) -> Result<Response, Failure> {
    let response = response.map_err(failure)?;
    if response.ok() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Err(Failure { status, text: if text.is_empty() { format!("{status} {}", response.status_text()) } else { text } })
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, Failure> {
    checked(Request::get(path).send().await).await?.json().await.map_err(failure)
}

pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, Failure> {
    let request = Request::post(path).json(body).map_err(failure)?;
    checked(request.send().await).await?.json().await.map_err(failure)
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T, Failure> {
    let request = Request::put(path).json(body).map_err(failure)?;
    checked(request.send().await).await?.json().await.map_err(failure)
}

pub async fn send<B: Serialize>(path: &str, body: &B) -> Result<(), Failure> {
    let request = Request::post(path).json(body).map_err(failure)?;
    checked(request.send().await).await.map(|_| ())
}

pub async fn delete(path: &str) -> Result<(), Failure> {
    checked(Request::delete(path).send().await).await.map(|_| ())
}

pub fn encode(text: &str) -> String {
    js_sys::encode_uri_component(text).into()
}
