use std::io::{self, BufRead};
unsafe extern "C" {
    fn plasmite_lite3_json_dec(json:*const u8, len:usize, buf:*mut u8, out:*mut usize, cap:usize)->i32;
    fn plasmite_lite3_get_val_ofs(buf:*const u8, len:usize, ofs:usize, key:*const u8, out:*mut usize)->i32;
    fn plasmite_lite3_json_enc(buf:*const u8,len:usize,ofs:usize,out:*mut usize)->*mut u8;
    fn plasmite_lite3_free(ptr:*mut u8);
}
fn main() {
    for input in io::stdin().lock().lines() {
        let input=input.unwrap();
        let envelope=format!("{{\"meta\":{{\"tags\":[]}},\"data\":{input}}}");
        let mut buf=vec![0u8; 1<<20]; let mut len=0; let mut offset=0;
        assert_eq!(unsafe {plasmite_lite3_json_dec(envelope.as_ptr(),envelope.len(),buf.as_mut_ptr(),&mut len,buf.len())},0);
        assert_eq!(unsafe {plasmite_lite3_get_val_ofs(buf.as_ptr(),len,0,c"data".as_ptr().cast(),&mut offset)},0);
        let mut text_len=0;
        let ptr=unsafe {plasmite_lite3_json_enc(buf.as_ptr(),len,offset,&mut text_len)};
        assert!(!ptr.is_null());
        let export=String::from_utf8(unsafe {std::slice::from_raw_parts(ptr,text_len)}.to_vec()).unwrap();
        unsafe {plasmite_lite3_free(ptr)};
        let report=match serde_json::from_str::<serde_json::Value>(&export) {
            Ok(value)=>serde_json::json!({"input":input,"export":export,"canonical":value.to_string()}),
            Err(error)=>serde_json::json!({"input":input,"export":export,"legacy_error":error.to_string()}),
        };
        println!("{report}");
    }
}
