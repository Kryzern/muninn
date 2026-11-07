// Muninn's renderer.
//
// Pass 1 draws the photo field into an offscreen target: every photo is a
// rounded quad whose depth blur comes from sampling its own mip chain with a
// bias, desaturated and dimmed toward the background the further back it sits.
// Pass 2 composites that target to the screen and puts Heimdall's glass on
// top wherever there is a glass shape (the search bar, a panel): the photos
// are frosted from the scene's mips, refracted outward at the rim with a
// slight red/blue split, and lit by a specular line that leans toward the
// pointer. Text is drawn afterwards with Direct2D.

// ---- photos -----------------------------------------------------------------

cbuffer Photo : register(b0)
{
    float4 pRect;   // x, y, w, h in px
    float2 pSize;   // target size, px
    float  pBias;   // extra mip bias: the depth blur
    float  pGray;   // 0 colour .. 1 grey
    float  pDim;    // 1 full brightness .. 0 background
    float  pRad;    // corner radius, px
    float  pSoft;   // edge softness, px
    float  pPad0;
    float2 pTexel;  // 1 / texture size
    float2 pPad1;
    float4 pBg;     // background colour
};

Texture2D    gTex  : register(t0);
SamplerState gSamp : register(s0);

struct QuadOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

QuadOut vs_quad(uint id : SV_VertexID)
{
    float2 c = float2(id & 1, id >> 1);
    float2 px = pRect.xy + c * pRect.zw;
    QuadOut o;
    o.pos = float4(px.x / pSize.x * 2.0 - 1.0, 1.0 - px.y / pSize.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}

float sdRound(float2 p, float2 c, float2 h, float r)
{
    float2 q = abs(p - c) - h + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

float4 ps_photo(QuadOut i) : SV_Target
{
    float2 p = i.uv * pRect.zw;
    float d = sdRound(p, pRect.zw * 0.5, pRect.zw * 0.5, pRad);
    float cov = 1.0 - smoothstep(-pSoft, pSoft, d);
    float3 c;
    [branch] if (pBias < 0.25)
    {
        // in focus: one sample is all it needs
        c = gTex.SampleBias(gSamp, i.uv, pBias).rgb;
    }
    else
    {
        float2 t = pTexel * exp2(pBias) * 0.7;
        c = gTex.SampleBias(gSamp, i.uv, pBias).rgb * 0.4
          + gTex.SampleBias(gSamp, i.uv + float2( t.x,  t.y), pBias).rgb * 0.15
          + gTex.SampleBias(gSamp, i.uv + float2(-t.x,  t.y), pBias).rgb * 0.15
          + gTex.SampleBias(gSamp, i.uv + float2( t.x, -t.y), pBias).rgb * 0.15
          + gTex.SampleBias(gSamp, i.uv - t, pBias).rgb * 0.15;
    }
    c = lerp(c, luma(c).xxx, pGray);
    c = lerp(pBg.rgb, c, pDim);
    return float4(c * cov, cov);
}

// ---- glass composite ----------------------------------------------------------

cbuffer Glass : register(b0)
{
    float2 uSize;       // target size, px
    float  uScale;      // DPI scale
    float  uSceneDim;   // 1 = scene as is, lower darkens it (behind panels)
    float2 uLight;      // unit vector toward the light
    float  uLens;       // rim refraction distance (px at 1.0 scale)
    float  uBlurLod;    // mip level for the frost
    float4 uShape[4];   // glass rectangles, px
    float4 uRadius;     // their corner radii, px
    float4 uAlpha;      // their visibility, 0 = absent
    float4 uTint;       // rgb, strength
    float4 uLensK;      // each shape's rim refraction, px at 1.0 scale: strong
                        // on the small search bar, gentle on big panels, which
                        // would otherwise smear the photo corners behind them
};

Texture2D gScene : register(t0);

float4 vs_full(uint id : SV_VertexID) : SV_Position
{
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
}

float3 at(float2 px, float lod) { return gScene.SampleLevel(gSamp, px / uSize, lod).rgb; }

float3 frost(float2 px, float lod)
{
    float2 t = exp2(lod) * 0.9;
    float3 s = at(px, lod) * 4.0;
    s += at(px + float2( t.x, 0.0), lod) * 2.0;
    s += at(px + float2(-t.x, 0.0), lod) * 2.0;
    s += at(px + float2(0.0,  t.y), lod) * 2.0;
    s += at(px + float2(0.0, -t.y), lod) * 2.0;
    s += at(px + t, lod);
    s += at(px - t, lod);
    s += at(px + float2(t.x, -t.y), lod);
    s += at(px + float2(-t.x, t.y), lod);
    return s / 16.0;
}

float hash(float2 p) { return frac(sin(dot(p, float2(12.9898, 78.233))) * 43758.5453); }

float shapeD(float2 px, int k)
{
    float4 s = uShape[k];
    return sdRound(px, s.xy + s.zw * 0.5, s.zw * 0.5, uRadius[k]);
}

// The glass of shape `k` at this pixel (inside it, at distance d < 0.5).
float3 glassAt(float2 px, int k, float d)
{
    float4 s = uShape[k];
    float2 c = s.xy + s.zw * 0.5, h = s.zw * 0.5;
    float r = uRadius[k];
    float2 e = float2(1.0, 0.0);
    float2 n = float2(sdRound(px + e.xy, c, h, r) - sdRound(px - e.xy, c, h, r),
                      sdRound(px + e.yx, c, h, r) - sdRound(px - e.yx, c, h, r));
    n = n / max(length(n), 1e-4);
    float depth = max(-d, 0.0);
    float t = saturate(1.0 - depth / (18.0 * uScale));
    float bend = t * t * t;

    float2 sp = px + n * bend * uLensK[k] * uScale;
    float3 col = frost(sp, uBlurLod) * uSceneDim;
    if (bend > 0.015)
    {
        float ca = bend * 3.0 * uScale;
        col.r = frost(sp + n * ca, uBlurLod).r * uSceneDim;
        col.b = frost(sp - n * ca, uBlurLod).b * uSceneDim;
    }
    col = lerp(luma(col).xxx, col, 1.3);
    // Darken over bright photos so text stays readable, judged from this
    // pixel's own (smooth) frost and eased, never clamped from a tiny mip.
    col *= lerp(0.85, 0.42, smoothstep(0.04, 0.75, luma(col)));
    col = lerp(col, uTint.rgb, uTint.a);

    float facing = saturate(dot(n, uLight));
    float away = saturate(dot(n, -uLight));
    float line_ = 1.0 - smoothstep(0.0, 1.4 * uScale, abs(depth - 0.6 * uScale));
    col += line_ * (0.10 + 0.70 * facing * facing + 0.22 * away * away * away) * 0.55;
    col += bend * (0.035 + 0.09 * facing) * 0.8;
    col += (hash(px) - 0.5) * (1.5 / 255.0);
    return saturate(col);
}

float4 ps_glass(float4 pos : SV_Position) : SV_Target
{
    float2 px = pos.xy;
    float3 outc = at(px, 0.0) * uSceneDim;

    // Every piece of glass is layered over what's below it, in order, each
    // blended by its own visibility. (Handing each pixel to the nearest shape
    // instead drew diagonal seams wherever two pieces overlapped, e.g. the bar
    // and a panel while one fades into the other.) Pixels clear of a shape's
    // box, plus its shadow, skip it.
    float m = 10.0 * uScale;
    [unroll] for (int j = 0; j < 4; j++)
    {
        float4 b = uShape[j];
        float a = uAlpha[j];
        if (a > 0.001 && px.x > b.x - m && px.y > b.y - m && px.x < b.x + b.z + m && px.y < b.y + b.w + m)
        {
            float d = shapeD(px, j);
            float inside = saturate(0.5 - d);
            // a faint contact shadow only: no dark halo
            outc *= 1.0 - exp(-max(d, 0.0) / (3.0 * uScale)) * 0.18 * a * (1.0 - inside);
            float cov = inside * a;
            if (cov > 0.0)
                outc = lerp(outc, glassAt(px, j, d), cov);
        }
    }
    return float4(outc, 1.0);
}
