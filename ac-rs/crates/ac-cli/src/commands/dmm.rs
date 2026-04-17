use crate::client::AcClient;
use super::check_ack;

pub fn run(client: &mut AcClient) {
    let ack = check_ack(
        client.send_cmd(&serde_json::json!({"cmd": "dmm_read"}), None),
        "dmm_read",
    );
    if let Some(idn) = ack.get("idn").and_then(|v| v.as_str()) {
        println!("\n  {idn}");
    }
    let vrms = ack
        .get("vrms")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let dbu = ac_core::conversions::vrms_to_dbu(vrms);
    let vpp = vrms * 2.0 * std::f64::consts::SQRT_2;

    if vrms >= 1.0 {
        println!("\n  AC  {vrms:.6} Vrms  =  {dbu:+.2} dBu  =  {vpp:.4} Vpp\n");
    } else {
        println!(
            "\n  AC  {:.4} mVrms  =  {dbu:+.2} dBu  =  {:.4} mVpp\n",
            vrms * 1000.0,
            vpp * 1000.0
        );
    }
}
