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

use hyper::client::{Client, HttpConnector};
use hyper::status::StatusCode;
use hyper::Url;

use futures::Future;
use futures::future::{ok, err};
use futures::sync::mpsc;
use futures::Sink;
use futures::stream::Stream;

use tokio_core::reactor;

use html5ever::parse_document;
use html5ever::rcdom::{self, RcDom};
use html5ever::Attribute;
use html5ever::tendril::TendrilSink;

use std::rc::Rc;

type HttpClient = Client<HttpConnector>;

quick_main!(run);

fn run() -> Result<()> {
    let root = std::env::args().nth(1).ok_or(ErrorKind::Msg(String::from("No URL provided")))?;
    let root = hyper::Url::parse(&root).chain_err(|| "Url failed to parse")?;
    let mut core = reactor::Core::new().chain_err(|| "Failed to open reactor")?;
    let handle = core.handle();


    let client = Client::new(&handle);
    let (tx, rx) = mpsc::channel(1024);
    // get root
    start_worker(&handle, &tx, &client, vec![root]);

    let mut ct = 0;

    let handler = rx.for_each(move |(page_url, links)| {
        let linkurls = links.iter().filter_map(|link| {
            hyper::Url::parse(&link).ok()
        }).collect::<Vec<_>>();

        ct += 1;
        println!("Found {} links in {}", linkurls.len(), page_url );
        println!("COUNT: {}", ct);
        if ct < 5 {
            start_worker(&handle, &tx, &client, linkurls);
        }
        ok(())
    });

    core.run(handler).unwrap();
    Ok(())
}

fn start_worker(handle: &reactor::Handle,
                tx: &mpsc::Sender<(Url, Vec<String>)>,
                client: &HttpClient,
                links: Vec<Url>) {
    for url in links {
        let tx = tx.clone();
        let worker = fetch_html(&client, url.clone())
            .then(|res| {
                match res {
                    Ok((StatusCode::Ok, body)) => ok((url, process_html(&body))),
                    Ok((status, _)) => {
                        println!("Bad status code {} for {}", status, url);
                        err(())
                    },
                    Err(e) => {
                        println!("Worker error: {} for {}", e, url);
                        err(())
                    }
                }
            })
            .and_then(|(url, links)| tx.send((url, links))
                      .then(|_| ok(())));
        handle.spawn(worker);
    }
}


fn fetch_html(client: &HttpClient, url: Url) -> impl Future<Item=(StatusCode, Vec<u8>), Error=hyper::Error> {
    println!("Fetching URL: {}", url);
    client.clone()
        .get(url)
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
}

fn process_html(source: &[u8]) -> Vec<String> {
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
