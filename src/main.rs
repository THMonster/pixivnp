use chrono::prelude::*;
use clap::Arg;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::TryFrom;
use tokio::io::AsyncWriteExt;

#[derive(Serialize, Deserialize, Debug)]
struct PxItemType {
    bl: bool,
    furry: bool,
    antisocial: bool,
    drug: bool,
}

#[derive(Serialize, Deserialize, Debug)]
struct PxItem {
    title: String,
    illust_id: u64,
    url: String,
    illust_page_count: String,
    illust_content_type: PxItemType,
}

#[derive(Serialize, Deserialize, Debug)]
struct PxResp {
    contents: Vec<PxItem>,
}

#[derive(Clone)]
struct PxPic {
    origin_url: String,
    title: String,
    page_cnt: usize,
    id: u64,
}
async fn download_pic_single(url: &str, file_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let mut resp = client
        .get(format!(r"{}", &url).as_str())
        .header(r"User-Agent", r"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/89.0.4389.82 Safari/537.36")
        .header(r#"Referer"#, r#"https://www.pixiv.net/ranking.php"#)
        .send().await?;
    if resp.status() != 200 {
        if resp.status() == 404 {
            if url.ends_with("jpg") {
                let url = format!("{}png", &url[..url.len() - 3]);
                let file_name = format!("{}png", &file_name[..file_name.len() - 3]);
                let mut resp = client
                    .get(format!(r"{}", &url).as_str())
                    .header(r"User-Agent", r"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/89.0.4389.82 Safari/537.36")
                    .header(r#"Referer"#, r#"https://www.pixiv.net/ranking.php"#)
                    .send().await?;
                if resp.status() != 200 {
                    println!("{}: {}", resp.status(), &url);
                } else {
                    let mut f =
                        tokio::fs::File::create(format!("./pixiv_pic/{}", &file_name)).await?;
                    while let Some(chunk) = resp.chunk().await? {
                        f.write_all(&chunk).await?;
                    }
                    f.sync_all().await?;
                }
            }
        } else {
            println!("{}: {}", resp.status(), &url);
        }
    } else {
        let mut f = tokio::fs::File::create(format!("./pixiv_pic/{}", &file_name)).await?;
        while let Some(chunk) = resp.chunk().await? {
            f.write_all(&chunk).await?;
        }
        f.sync_all().await?;
    }
    Ok(())
}

async fn download_worker(
    rx: async_channel::Receiver<PxPic>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let pic = rx.recv().await?;
        let mut cnt = pic.page_cnt;
        while cnt > 0 {
            let url = format!(r"{}p{}.jpg", pic.origin_url.as_str(), cnt - 1);
            let filename = format!(r"{}_p{}.jpg", pic.id, cnt - 1);
            while let Err(e) = download_pic_single(url.as_str(), filename.as_str()).await {
                println!("{}: {:?}", pic.title.as_str(), e);
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            }
            cnt -= 1;
        }
        println!("remain: {}", rx.len());
        if rx.len() == 0 {
            println!("no more pic, exit.");
            break;
        }
    }
    Ok(())
}

async fn get_pic_single_day(
    rank_date: &str,
) -> Result<HashMap<u64, PxPic>, Box<dyn std::error::Error>> {
    let mut ret: HashMap<u64, PxPic> = HashMap::new();
    let re = Regex::new(r"(/img/\d{4}/\d{2}/\d{2}/\d{2}/\d{2}/\d{2}/\d+)_p").unwrap();
    let client = reqwest::Client::new();
    for i in 1u8..5u8 {
        let resp = client
        .get(r#"https://www.pixiv.net/ranking.php?mode=daily&content=illust&format=json"#)
        .header(r#"User-Agent"#, r#"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/89.0.4389.82 Safari/537.36"#)
        .header(r#"Referer"#, r#"https://www.pixiv.net/ranking.php"#)
        .query(&[("p", format!("{:?}",i).as_str()), ("date", rank_date)])
        .send().await?
        .json::<PxResp>().await?;
        // .json::<HashMap<contents: String,Vec<PxItem>>>().await?;
        for item in resp.contents.into_iter() {
            if item.illust_content_type.bl == true
                || item.illust_content_type.furry == true
                || item.illust_content_type.antisocial == true
                || item.illust_content_type.drug == true
            {
                continue;
            }
            let caps = re.captures(&item.url).unwrap();
            let p = PxPic {
                origin_url: format!(
                    "https://i.pximg.net/img-original{}_",
                    caps.get(1).unwrap().as_str()
                ),
                title: item.title,
                page_cnt: item.illust_page_count.parse().unwrap_or(1),
                id: item.illust_id,
            };
            if p.page_cnt <= 5 {
                ret.insert(item.illust_id, p);
            }
        }
    }
    Ok(ret)
}

async fn get_pic(day_start: u8, mut day_range: u8) -> Result<(), Box<dyn std::error::Error>> {
    let mut tnow = Local::now();
    let mut all_pic: HashMap<u64, PxPic> = HashMap::new();
    tnow = tnow - chrono::Duration::days(i64::try_from(day_start)?);
    while day_range > 0 {
        all_pic.extend(
            get_pic_single_day(
                format!("{:04}{:02}{:02}", tnow.year(), tnow.month(), tnow.day()).as_str(),
            )
            .await?,
        );
        tnow = tnow - chrono::Duration::days(1);
        day_range -= 1;
    }
    println!("total: {:?}", all_pic.len());

    let (tx, rx) = async_channel::unbounded();
    let mut workers = Vec::new();
    for _ in 1..20 {
        let tmprx = rx.clone();
        let f = tokio::task::spawn_local(async move {
            download_worker(tmprx).await.unwrap();
        });
        workers.push(f);
    }
    for (_, v) in all_pic.drain() {
        tx.send(v).await?;
    }
    for join_handle in workers.drain(..) {
        join_handle.await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let ma = clap::App::new("pixivnp")
        .author("IsoaSFlus <me@isoasflus.com>")
        .arg(
            Arg::with_name("day-start")
                .short("d")
                .long("day-start")
                .value_name("INT")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("day-length")
                .short("l")
                .long("day-length")
                .value_name("INT")
                .required(true)
                .takes_value(true),
        )
        .get_matches();
    let day_start: u8 = ma.value_of("day-start").unwrap_or("0").parse().unwrap_or(0);
    let day_length: u8 = ma
        .value_of("day-length")
        .unwrap_or("1")
        .parse()
        .unwrap_or(1);

    // This is running on a core thread.
    std::fs::create_dir_all("./pixiv_pic/").unwrap();

    let local = tokio::task::LocalSet::new();

    // Run the local task set.
    local
        .run_until(async move {
            tokio::task::spawn_local(async move {
                get_pic(day_start, day_length).await.unwrap();
            })
            .await
            .unwrap();
        })
        .await;
}
