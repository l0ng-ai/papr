//! Mobile originals open through the OS browser; article content stays in Papr.
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
#[tauri::command]
pub async fn open_page_view(app: AppHandle, url: String, x:f64,y:f64,width:f64,height:f64,zoom:Option<f64>) -> Result<(),String> {
    let _=(x,y,width,height,zoom);
    let parsed=url::Url::parse(&url).map_err(|e|e.to_string())?;
    if !matches!(parsed.scheme(),"http"|"https") {return Err("invalid article URL".into());}
    app.opener().open_url(url,None::<&str>).map_err(|e|e.to_string())
}
#[tauri::command]
pub async fn set_page_view_bounds(x:f64,y:f64,width:f64,height:f64)->Result<(),String>{let _=(x,y,width,height);Ok(())}
#[tauri::command]
pub async fn set_page_view_visible(visible:bool)->Result<(),String>{let _=visible;Ok(())}
#[tauri::command]
pub async fn close_page_view()->Result<(),String>{Ok(())}
#[tauri::command]
pub async fn set_page_view_zoom(zoom:f64)->Result<(),String>{let _=zoom;Ok(())}
