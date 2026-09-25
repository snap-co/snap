//! Web Crypto RS256 implementation. Private PKCS8 bytes live in issuer Store;
//! imported CryptoKeys are isolate-local and reconstructed after eviction.
use serde_json::Value;
use snap_http::Response;
use snap_oidc::storage::{SETTINGS, read, row, text, transaction};
use snap_store::{Predicate as P, Query, Statement as S, Store};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = "
function enc(b) { let s=''; for(const x of new Uint8Array(b)) s+=String.fromCharCode(x); return btoa(s).replace(/=/g,'').replace(/\\+/g,'-').replace(/\\//g,'_'); }
function dec(s) { return Uint8Array.from(atob(s.replace(/-/g,'+').replace(/_/g,'/')), c=>c.charCodeAt(0)); }
export async function snapOidcGenerate() {
 const pair=await crypto.subtle.generateKey({name:'RSASSA-PKCS1-v1_5',modulusLength:2048,publicExponent:new Uint8Array([1,0,1]),hash:'SHA-256'},true,['sign','verify']);
 return enc(await crypto.subtle.exportKey('pkcs8',pair.privateKey));
}
export async function snapOidcImport(s) { return crypto.subtle.importKey('pkcs8',dec(s),{name:'RSASSA-PKCS1-v1_5',hash:'SHA-256'},true,['sign']); }
export async function snapOidcPublic(key) {
 const k=await crypto.subtle.exportKey('jwk',key);
 const kid=enc(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(k.n)));
 return JSON.stringify({keys:[{kty:'RSA',use:'sig',alg:'RS256',kid,n:k.n,e:k.e}]});
}
export async function snapOidcSign(key,kid,claims) {
 const h=enc(new TextEncoder().encode(JSON.stringify({typ:'JWT',alg:'RS256',kid})));
 const p=enc(new TextEncoder().encode(claims));const message=h+'.'+p;
 return message+'.'+enc(await crypto.subtle.sign('RSASSA-PKCS1-v1_5',key,new TextEncoder().encode(message)));
}
export async function snapOidcVerify(token,keys) {
 if(token.length>16384) throw Error('Invalid token');
 const p=token.split('.');if(p.length!==3) throw Error('Invalid token');
 const h=JSON.parse(new TextDecoder().decode(dec(p[0])));
 if(h.alg!=='RS256'||h.crit!==undefined||typeof h.kid!=='string') throw Error('Invalid algorithm');
 const matches=JSON.parse(keys).keys.filter(k=>k.kid===h.kid);if(matches.length!==1) throw Error('Unknown key');
 const k=matches[0];if(k.kty!=='RSA'||k.use!=='sig'||k.alg!=='RS256'||typeof k.n!=='string'||typeof k.e!=='string'||dec(k.n).length<256||dec(k.n).length>512||dec(k.e).length>8) throw Error('Invalid key');
 const key=await crypto.subtle.importKey('jwk',k,{name:'RSASSA-PKCS1-v1_5',hash:'SHA-256'},false,['verify']);
 if(!await crypto.subtle.verify('RSASSA-PKCS1-v1_5',key,dec(p[2]),new TextEncoder().encode(p[0]+'.'+p[1]))) throw Error('Invalid signature');
 return new TextDecoder().decode(dec(p[1]));
}
")]
extern "C" {
    #[wasm_bindgen(catch,js_name=snapOidcGenerate)]
    async fn generate() -> Result<String, JsValue>;
    #[wasm_bindgen(catch,js_name=snapOidcImport)]
    async fn import(encoded: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch,js_name=snapOidcPublic)]
    async fn public(key: &JsValue) -> Result<String, JsValue>;
    #[wasm_bindgen(catch,js_name=snapOidcSign)]
    async fn sign(key: &JsValue, kid: &str, claims: &str) -> Result<String, JsValue>;
    #[wasm_bindgen(catch,js_name=snapOidcVerify)]
    async fn verify_raw(token: &str, keys: &str) -> Result<String, JsValue>;
}
#[derive(Clone)]
pub struct Crypto {
    key: JsValue,
    jwks: Value,
}
impl Crypto {
    pub async fn load(store: &impl Store) -> Result<Self, Response> {
        let q = Query::new(SETTINGS)
            .matching(vec![P::eq("key", "rs256")])
            .limit(1);
        if read(store, q.clone()).await?.is_none() {
            let encoded = generate().await.map_err(|_| snap_oidc::unavailable())?;
            transaction(
                store,
                vec![],
                vec![S::Insert {
                    table: SETTINGS,
                    row: row(&[("key", "rs256".into()), ("value", encoded.into())]),
                }],
            )
            .await?;
        }
        let record = read(store, q).await?.ok_or_else(snap_oidc::unavailable)?;
        let key = import(&text(&record, "value")?)
            .await
            .map_err(|_| snap_oidc::unavailable())?;
        let jwks = serde_json::from_str(&public(&key).await.map_err(|_| snap_oidc::unavailable())?)
            .map_err(|_| snap_oidc::unavailable())?;
        Ok(Self { key, jwks })
    }
}
impl snap_oidc::Crypto for Crypto {
    fn random(&self) -> Result<String, Response> {
        crate::crypto::random().map_err(|_| snap_oidc::unavailable())
    }
    fn jwks(&self) -> Value {
        self.jwks.clone()
    }
    async fn sign(&self, claims: Value) -> Result<String, Response> {
        sign(
            &self.key,
            self.jwks["keys"][0]["kid"]
                .as_str()
                .ok_or_else(snap_oidc::unavailable)?,
            &claims.to_string(),
        )
        .await
        .map_err(|_| snap_oidc::unavailable())
    }
    async fn verify(&self, token: String) -> Result<Value, Response> {
        verify(&token, &self.jwks).await
    }
}
pub async fn verify(token: &str, jwks: &Value) -> Result<Value, Response> {
    let json = verify_raw(token, &jwks.to_string())
        .await
        .map_err(|_| Response::error(400, "invalid_token", "Invalid ID token signature"))?;
    serde_json::from_str(&json).map_err(|_| snap_oidc::unavailable())
}
