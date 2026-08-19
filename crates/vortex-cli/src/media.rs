//! `vortex media` — the media pipeline, in this process, with no daemon anywhere.
//!
//! The same harness argument as [`crate::fetch`]: when a video comes out silent or won't
//! open, the first question is whether the parser, the fetcher or ffmpeg is responsible,
//! and one command should answer it. With no `--variant` and no `--height` it only reads
//! the manifest and prints the ladder, which is also the fastest way to see what a site is
//! actually serving.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use vortex_engine::job::{self, Engine, Interrupted};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_media::job::MediaSpec;
use vortex_proto::{fmt, ContainerPreference, MediaCandidate, MediaSelection, RequestEnvelope};

pub struct Options {
    pub url: String,
    pub variant: Option<String>,
    pub height: Option<u32>,
    pub dir: String,
    pub name: Option<String>,
    pub subtitles: bool,
    pub container: ContainerPreference,
    pub connections: u8,
}

pub async fn run(options: Options) -> Result<()> {
    let engine = Arc::new(Engine::new(Transport::new(TransportConfig::default())));
    let envelope = RequestEnvelope::new(&options.url);
    let preferred = options.height.unwrap_or(1080);

    let candidate = match vortex_media::plan::inspect(&engine, &envelope, preferred).await {
        Ok(candidate) => candidate,
        Err(e) => {
            // yt-dlp is the fallback, not the first answer — and never after a refusal.
            if matches!(e, vortex_media::Error::Refused(_)) {
                anyhow::bail!("{}", e.user_message());
            }
            match vortex_media::ytdlp::YtDlp::find() {
                Some(tool) => vortex_media::ytdlp::extract(&tool, &envelope, preferred)
                    .await
                    .map_err(|_| anyhow::anyhow!("{}", e.user_message()))?,
                None => anyhow::bail!("{}", e.user_message()),
            }
        }
    };

    if options.variant.is_none() && options.height.is_none() {
        describe(&candidate);
        return Ok(());
    }

    let index = match &options.variant {
        Some(id) => candidate
            .variants
            .iter()
            .position(|v| &v.id == id)
            .ok_or_else(|| anyhow::anyhow!("no variant with id {id}"))?,
        None => candidate.default_variant.unwrap_or(0) as usize,
    };
    let variant = candidate
        .variants
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("that stream has no variants"))?;

    let selection = MediaSelection {
        manifest_url: candidate.manifest_url.clone(),
        kind: candidate.kind,
        title: candidate.title.clone(),
        variant_id: variant.id.clone(),
        // Highest-bitrate rendition in the selected variant's group, defaulting to the one
        // the manifest marks as default (04 §3).
        audio_id: candidate
            .audio
            .iter()
            .find(|track| track.default)
            .or_else(|| candidate.audio.first())
            .map(|track| track.id.clone()),
        subtitle_ids: if options.subtitles {
            candidate.subtitles.iter().map(|t| t.id.clone()).collect()
        } else {
            Vec::new()
        },
        container: options.container,
    };

    eprintln!(
        "{} \u{b7} {} \u{b7} {}{}",
        candidate.title,
        quality(variant),
        candidate
            .duration_secs
            .map(|d| fmt::eta(d as u32))
            .unwrap_or_else(|| "unknown length".to_owned()),
        match &selection.audio_id {
            Some(_) => " \u{b7} separate audio",
            None => "",
        }
    );

    let (events, mut incoming) = mpsc::channel(64);
    let (control, control_rx) = job::control();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nstopping \u{2014} run the same command again to resume");
            control.stop();
        }
    });
    tokio::spawn(async move {
        let mut reporter = crate::fetch::Reporter::default();
        while let Some(event) = incoming.recv().await {
            reporter.observe(event);
        }
    });

    let outcome = vortex_media::job::transfer(
        engine,
        MediaSpec {
            envelope,
            selection,
            dest_dir: PathBuf::from(&options.dir),
            filename: options.name.clone(),
            max_connections: options.connections,
            container: options.container,
        },
        events,
        control_rx,
    )
    .await;

    match outcome {
        Ok(done) => {
            println!(
                "\n{} in {}\n{}",
                fmt::bytes(done.bytes),
                fmt::eta(done.elapsed.as_secs() as u32),
                done.path.display()
            );
            if done.retries.0 > 0 {
                println!("{} segments were unavailable", done.retries.0);
            }
            Ok(())
        }
        Err(Interrupted::Stopped) => {
            println!("\nstopped \u{2014} the segments already fetched are still there");
            Ok(())
        }
        Err(Interrupted::NeedsDecision(decision)) => anyhow::bail!("{}", crate::fetch::describe(&decision)),
        Err(Interrupted::Failed(e)) => anyhow::bail!("{}", e.user_message()),
    }
}

fn describe(candidate: &MediaCandidate) {
    println!(
        "{}  \u{b7}  {}  \u{b7}  {:?}{}",
        candidate.title,
        candidate
            .duration_secs
            .map(|d| fmt::eta(d as u32))
            .unwrap_or_else(|| "unknown length".to_owned()),
        candidate.kind,
        if candidate.live { "  \u{b7}  live" } else { "" }
    );

    println!("\nvideo");
    for (index, variant) in candidate.variants.iter().enumerate() {
        let default = if Some(index as u32) == candidate.default_variant {
            " <- default"
        } else {
            ""
        };
        println!(
            "  {:<10} {:>10} {:>10}  {}{default}",
            quality(variant),
            fmt::rate(variant.bandwidth / 8),
            variant
                .estimated_bytes
                .map(|b| format!("~{}", fmt::bytes(b)))
                .unwrap_or_default(),
            variant.codec_label.as_deref().unwrap_or("")
        );
        println!("             id {}", variant.id);
    }

    if !candidate.audio.is_empty() {
        println!("\naudio");
        for track in &candidate.audio {
            println!(
                "  {:<20} {}{}",
                track.label,
                track.language.as_deref().unwrap_or(""),
                if track.default { "  <- default" } else { "" }
            );
        }
    }
    if !candidate.subtitles.is_empty() {
        println!("\nsubtitles");
        for track in &candidate.subtitles {
            println!("  {:<20} {}", track.label, track.language.as_deref().unwrap_or(""));
        }
    }
}

fn quality(variant: &vortex_proto::MediaVariant) -> String {
    match (variant.width, variant.height) {
        (_, Some(height)) => format!("{height}p"),
        (Some(width), None) => format!("{width}w"),
        _ => format!("{} kbps", variant.bandwidth / 1000),
    }
}
