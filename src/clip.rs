//! OpenCLIP on ONNX Runtime (any of the models in models.json): fp16 on any DirectX 12 GPU via DirectML, fp32 on the CPU if that fails.

use anyhow::{anyhow, Result};
use ort::environment::Environment;
use ort::ep;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::path::Path;
use std::sync::Mutex;
use tokenizers::Tokenizer;

pub const SIZE: usize = 224;
const CTX: usize = 77;
const EOT: u32 = 49407;
pub const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
pub const STD: [f32; 3] = [0.268_629_54, 0.261_302_58, 0.275_777_11];

pub struct Clip {
    image: Mutex<Session>,
    text: Mutex<Session>,
    tokenizer: Tokenizer,
    /// Where inference runs, e.g. "NVIDIA GeForce RTX 2070" or "CPU".
    pub device: String,
    /// Length of an embedding: 512 for the B models, 768 for L/14.
    pub dim: usize,
    /// Prompt embeddings for telling text-heavy images from plain photos.
    texty: Vec<f32>,
    plain: Vec<f32>,
    _env: Environment,
}

fn session(env: &Environment, path: &Path, gpu: Option<i32>) -> Result<Session> {
    let e = |x: ort::Error<_>| anyhow!("{x}");
    let dbg = |x: anyhow::Error| { if std::env::var_os("MUNINN_DEBUG").is_some() { eprintln!("session: {x}"); } x };
    let b = Session::builder(env)?.with_optimization_level(GraphOptimizationLevel::Level3).map_err(e)?;
    let mut b = if let Some(id) = gpu {
        // DirectML wants these off
        b.with_memory_pattern(false).map_err(e)?.with_parallel_execution(false).map_err(e)?
            .with_execution_providers([ep::DirectML::default().with_device_id(id).build().error_on_failure()]).map_err(e)?
    } else {
        b.with_execution_providers([ep::CPU::default().build()]).map_err(e)?
    };
    b.commit_from_file(path).map_err(|x| dbg(anyhow!("{x}")))
}

impl Clip {
    pub fn load(dir: &Path, cpu: bool) -> Result<Self> {
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
        // onnxruntime.dll ships next to the exe (or the models in a dev tree)
        let dll = [std::env::current_exe()?.with_file_name("onnxruntime.dll"), dir.join("../onnxruntime.dll")]
            .into_iter()
            .find(|p| p.exists())
            .ok_or_else(|| anyhow!("onnxruntime.dll not found next to muninn.exe"))?;
        let env = ort::init_from(&dll).map_err(|e| anyhow!("{e}"))?.with_name("muninn").build()?;
        let force_cpu = cpu || std::env::var_os("MUNINN_CPU").is_some();
        // best GPU first, then Windows' software adapter, then the CPU if its model is installed
        let adapters = if force_cpu { vec![] } else { crate::gpu::ranked() };
        for a in adapters {
            // both models load at once
            let (image, text) = std::thread::scope(|sc| {
                let t = sc.spawn(|| session(&env, &dir.join("text-f16.onnx"), Some(a.index)));
                let i = session(&env, &dir.join("image-f16.onnx"), Some(a.index));
                (i, t.join().unwrap_or_else(|_| Err(anyhow!("text model thread panicked"))))
            });
            let (Ok(image), Ok(text)) = (image, text) else {
                if std::env::var_os("MUNINN_DEBUG").is_some() { eprintln!("{} unavailable", a.name); }
                continue;
            };
            let device = if a.software { "CPU (software DirectX)".to_string() } else { a.name };
            return Self { image: Mutex::new(image), text: Mutex::new(text), tokenizer, device, dim: 0, texty: vec![], plain: vec![], _env: env }.prompts();
        }
        let image = session(&env, &dir.join("image-f32.onnx"), None)?;
        let text = session(&env, &dir.join("text-f32.onnx"), None)?;
        Self { image: Mutex::new(image), text: Mutex::new(text), tokenizer, device: "CPU".into(), dim: 0, texty: vec![], plain: vec![], _env: env }.prompts()
    }

    fn prompts(mut self) -> Result<Self> {
        self.dim = self.embed_text("a photo")?.len();
        const TEXTY: [&str; 12] = ["text", "a screenshot", "a document", "a receipt", "a sign with words", "a menu", "a poster", "a page of a book", "a whiteboard", "a label", "handwriting", "a chart"];
        const PLAIN: [&str; 7] = ["a photo", "a landscape", "a person", "an animal", "food", "a building", "nature"];
        for t in TEXTY { let v = self.embed_text(t)?; self.texty.extend(v); }
        for t in PLAIN { let v = self.embed_text(t)?; self.plain.extend(v); }
        Ok(self)
    }

    /// Above zero-ish when the image looks like it holds text worth reading.
    pub fn textness(&self, v: &[f32]) -> f32 {
        let best = |m: &[f32]| m.chunks_exact(self.dim).map(|p| p.iter().zip(v).map(|(a, b)| a * b).sum::<f32>()).fold(f32::MIN, f32::max);
        best(&self.texty) - best(&self.plain)
    }

    /// `pixels`: normalized CHW floats, `n` images back to back. Returns unit vectors.
    pub fn embed_images(&self, pixels: Vec<f32>, n: usize) -> Result<Vec<f32>> {
        let x = Tensor::from_array(([n, 3, SIZE, SIZE], pixels))?;
        let mut s = self.image.lock().unwrap();
        let out = s.run(ort::inputs!["pixels" => x])?;
        let (_, v) = out["embeds"].try_extract_tensor::<f32>()?;
        Ok(v.to_vec())
    }

    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let enc = self.tokenizer.encode(text.to_lowercase(), true).map_err(|e| anyhow!("{e}"))?;
        let mut ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        ids.truncate(CTX);
        if ids.len() == CTX {
            ids[CTX - 1] = EOT as i64;
        }
        ids.resize(CTX, 0);
        let x = Tensor::from_array(([1, CTX], ids))?;
        let mut s = self.text.lock().unwrap();
        let out = s.run(ort::inputs!["ids" => x])?;
        let (_, v) = out["embeds"].try_extract_tensor::<f32>()?;
        Ok(v.to_vec())
    }
}
