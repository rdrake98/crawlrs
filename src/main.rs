#![feature(conservative_impl_trait)]
#![feature(field_init_shorthand)]

extern crate hyper;
extern crate tokio_core;
extern crate tokio_timer;
extern crate futures;
extern crate html5ever;

#[macro_use]
extern crate error_chain;

mod errors {
    error_chain!{}
}

use errors::*;

use hyper::client::{Client, HttpConnector};
use hyper::status::StatusCode;
use hyper::Url;

use futures::{Future, Sink, IntoFuture, Stream};
use futures::future::{ok, err};
use futures::sync::mpsc;

use tokio_core::reactor;

use html5ever::parse_document;
use html5ever::rcdom::{self, RcDom};
use html5ever::Attribute;
use html5ever::tendril::TendrilSink;

use std::sync::atomic::{AtomicUsize, ATOMIC_USIZE_INIT, Ordering};
use std::collections::HashMap;

type HttpClient = Client<HttpConnector>;

quick_main!(run);

static REQUESTCT: AtomicUsize = ATOMIC_USIZE_INIT;
static mut MAXREQ: usize = 0;

fn maxreq() -> usize {
    unsafe { MAXREQ }
}

fn set_maxreq(v: usize) {
    unsafe { MAXREQ = v }
}

fn run() -> Result<()> {
    let root = std::env::args().nth(1).ok_or(ErrorKind::Msg(String::from("No URL provided")))?;
    set_maxreq(std::env::args()
        .nth(2)
        .map(|s| s.parse::<usize>().chain_err(|| "Failed to parse"))
        .unwrap_or(Ok(10))?);

    let root = hyper::Url::parse(&root).chain_err(|| "Url failed to parse")?;
    let mut core = reactor::Core::new().chain_err(|| "Failed to open reactor")?;
    let handle = core.handle();


    let client = Client::new(&handle);
    let (tx, rx) = mpsc::channel(1024);
    // get root
    start_worker(&handle, &tx, &client, vec![root]);

    let mut seen_urls = HashMap::new();

    let handler = rx.for_each(move |outcome| {
        let mut linkurls = Vec::new();
        for link in outcome.links {
            match hyper::Url::parse(&link) {
                Ok(url) => linkurls.push(url),
                Err(_) => {
                    let fixedurl = outcome.url.clone().join(&link).unwrap();
                    println!("Exapanded {} to {}", link, fixedurl);
                    linkurls.push(fixedurl);
                }
            }
        }
        for link in &linkurls {
            if link.as_str().ends_with(".js") {
                println!("js found: {}", link)
            }
        }
        let nlinks = linkurls.len();
        seen_urls.insert(outcome.url.clone(), linkurls.clone());
        let mut newlinks: Vec<_> = linkurls.into_iter().filter(|key| !seen_urls.contains_key(key)).collect();
        newlinks.sort();
        newlinks.dedup();
        println!("Found {} links in {}, of which {} are newnique", nlinks, outcome.url, newlinks.len() );
        start_worker(&handle, &tx, &client, newlinks);
        ok(())
    });

    core.run(handler).unwrap();
    Ok(())
}

fn start_worker(handle: &reactor::Handle,
                tx: &mpsc::Sender<ParseOutcome>,
                client: &HttpClient,
                links: Vec<Url>) {
    for url in links {
        if REQUESTCT.load(Ordering::Relaxed) > maxreq() { return }
        match url.scheme() {
            "https" => {
                // skip for now, hopefully soon won't be necessary
                println!("Skipping HTTPS: {}", url);
                continue
            },
            "http" => (),
            _rest => {
                println!("Skipping url {} (unrecognized scheme)", url);
                continue
            }
        };
        let tx = tx.clone();
        let worker = fetch_html(&client, url)
            .and_then(|outcome| tx.send(outcome)
                      .map_err(|e| format!("{}", e).into()))
            .then(|res| {
                if let Err(e) = res { log_error(e) };
                ok(())
            });
        let rq = REQUESTCT.load(Ordering::Relaxed);
        if rq < maxreq() {
            println!("Spawning {}th worker", rq);
            handle.spawn(worker);
        };
        REQUESTCT.fetch_add(1, Ordering::Relaxed);
    }
}


fn fetch_html(client: &HttpClient, url: Url) ->
    impl Future<Item=ParseOutcome, Error=Error>
{
    println!("Fetching URL: {}", url);
    client.clone()
        .get(url.clone())
        .and_then(|res| {
            let status = *res.status();
            let bodybuf = {
                let headers = res.headers();
                let buflen = headers.get::<hyper::header::ContentLength>();
                Vec::with_capacity(buflen.map(|b| b.0 as usize).unwrap_or(1024))
            };
            res.body().fold(bodybuf, |mut bodybuf, chunk| {
                bodybuf.extend_from_slice(&*chunk);
                ok::<_, hyper::Error>(bodybuf)
            }).map(move |b| (status, b))
        })
        .then(|res| {
            match res {
                Ok((s @ StatusCode::Ok, body))
                    | Ok((s @ StatusCode::Found, body))
                    | Ok((s @ StatusCode::MovedPermanently, body)) => {
                        process_html(&body)
                            .map(|links| ParseOutcome::new(url, s, links))
                            .into_future()
                    },
                Ok((status, _)) => err(format!(
                    "Bad status code {} for {}", status, url).into()),
                Err(e) => err(format!(
                    "Worker error: {} for {}", e, url).into())
            }
        })
}

fn process_html(source: &[u8]) -> Result<Vec<String>> {
    let dom = parse_html(source)?;
    Ok(find_links(&dom))
}

fn parse_html(mut source: &[u8]) -> Result<RcDom> {
    parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut source)
        .chain_err(|| "HTML parse failed")
}


fn find_links(dom: &RcDom) -> Vec<String> {
    let mut links = Vec::new();
    walk(&dom.document, 0, &mut links);
    links
}

fn walk(handle: &rcdom::Handle, depth: usize, links: &mut Vec<String>) {
    use html5ever::rcdom::NodeEnum::*;
    let node = handle.borrow();
    match node.node {
        Element(ref name ,_, ref attrs) => {
            if &*name.local == "a" {
                if let Some(url) = get_url(attrs) {
                    links.push(String::from(url));
                }
            }
        },
        _ => {}
    };
    for child in &node.children {
        walk(child, depth + 1, links);
    }
}

fn get_url(attrs: &[Attribute]) -> Option<&str> {
    for attr in attrs {
        if &*attr.name.local == "href" {
            return Some(&attr.value)
        }
    }
    return None
}

fn log_error(e: Error) {
    // taken from https://github.com/brson/error-chain/blob/master/examples/quickstart.rs
    use ::std::io::Write;
    let stderr = &mut ::std::io::stderr();
    let errmsg = "Error writing to stderr";

    writeln!(stderr, "error: {}", e).expect(errmsg);

    for e in e.iter().skip(1) {
        writeln!(stderr, "caused by: {}", e).expect(errmsg);
    }

    // The backtrace is not always generated. Try to run this example
    // with `RUST_BACKTRACE=1`.
    if let Some(backtrace) = e.backtrace() {
        writeln!(stderr, "backtrace: {:?}", backtrace).expect(errmsg);
    }
}

struct ParseOutcome {
    pub url: Url,
    pub status: StatusCode,
    pub links: Vec<String>
}

impl ParseOutcome {
    fn new(url: Url, status: StatusCode, links: Vec<String>) -> ParseOutcome {
        ParseOutcome { url, status, links }
    }
}
