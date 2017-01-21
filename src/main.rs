#![feature(conservative_impl_trait)]

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

use hyper::Client;
use hyper::client::HttpConnector;
use hyper::Url;

use futures::Future;
use futures::future::ok;
use futures::sync::mpsc;
use futures::Sink;
use futures::stream::Stream;

use tokio_core::reactor;

use html5ever::parse_document;
use html5ever::rcdom::{self, RcDom};
use html5ever::Attribute;
use html5ever::tendril::TendrilSink;

quick_main!(run);

const ROOT: &'static str = "http://www.gutenberg.org";

fn run() -> Result<()> {
    let mut core = tokio_core::reactor::Core::new().chain_err(|| "Failed to open reactor")?;
    let handle = core.handle();


    let client = Client::new(&handle);
    let (tx, rx) = mpsc::channel(1024);
    // start root
    start_worker(&handle, &tx, &client, vec![String::from(ROOT)]);

    let mut ct = 0;

    let handler = rx.for_each(move |links| {
        ct += 1;
        println!("{:?}", links);
        println!("COUNT: {}", ct);
        if ct < 5 {
            start_worker(&handle, &tx, &client, links);
        }
        ok(())
    });

    core.run(handler).unwrap();
    Ok(())
}

    fn start_worker(handle: &reactor::Handle,
                    tx: &mpsc::Sender<Vec<String>>,
                    client: &Client<HttpConnector>,
                    links: Vec<String>) {
    // start the root worker
    for link in links {
        let url = match hyper::Url::parse(&link) {
            Ok(url) => url,
            Err(_) => {
                println!("Invalid URL: {}", link);
                continue
            }
        };
        let tx = tx.clone();
        let worker = fetch_html(&client, url)
            .map(|buf| process(&buf))
            .map_err(|_| ())
            .and_then(move |links| tx.send(links)
                    .then(|_| ok(())));
        handle.spawn(worker);
    }
}


fn fetch_html(client: &Client<HttpConnector>, url: Url) -> impl Future<Item=Vec<u8>, Error=hyper::Error> {
    println!("Fetching URL: {}", url);
    client.clone()
        .get(url)
        .and_then(|res| {
            let buf = {
                let headers = res.headers();
                let buflen = headers.get::<hyper::header::ContentLength>();
                Vec::with_capacity(buflen.map(|b| b.0 as usize).unwrap_or(1024))
            };

            res.body().fold(buf, |mut buf, chunk| {
                buf.extend_from_slice(&*chunk);
                ok::<_, hyper::Error>(buf)
            })
        })
}

fn process(source: &[u8]) -> Vec<String> {
    let dom = parse_html(source);
    find_links(&dom)
}

fn parse_html(mut source: &[u8]) -> RcDom {
    parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut source)
        .unwrap()
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
