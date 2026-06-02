use super::check_ack;
use crate::client::AcClient;

pub fn run(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "dmm_read"}), None),
        "dmm_read",
    );
    if let Some(idn) = ack.get("idn").and_then(|v| v.as_str()) {
        println!("\n  {idn}");
    }
    let vrms = ack.get("vrms").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let dbu = ac_core::shared::conversions::vrms_to_dbu(vrms);

    println!(
        "\n  AC  {}  =  {dbu:+.2} dBu  =  {}\n",
        ac_core::shared::conversions::fmt_vrms(vrms),
        ac_core::shared::conversions::fmt_vpp(vrms)
    );
}
