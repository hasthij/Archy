//! Shows what Windows' speech recognizer sees: language, states, audio
//! problems and guesses. Speak your phrase while it runs.
//! cargo run -p archy-core --example voice_diag
use windows::core::{Ref, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::SpeechRecognition::*;

fn main() -> windows::core::Result<()> {
    archy_core::init_thread()?;
    let recognizer = SpeechRecognizer::new()?;
    let mut list: Vec<String> = std::env::args().skip(1).filter(|a| a != "--eco").collect();
    if std::env::args().any(|a| a == "--eco") {
        let e = archy_core::eco::apply();
        println!(
            "efficiency mode ON ({} of {} cores)",
            e.efficiency_cores, e.total_cores
        );
    }
    if list.is_empty() {
        list = vec![
            "archy clip that".into(),
            "archy clip the last minute".into(),
        ];
    }
    println!("phrases: {list:?}");
    let phrases: windows_collections::IIterable<HSTRING> =
        list.iter().map(HSTRING::from).collect::<Vec<_>>().into();
    recognizer
        .Constraints()?
        .Append(&SpeechRecognitionListConstraint::Create(&phrases)?)?;
    let compiled = recognizer.CompileConstraintsAsync()?.join()?;
    println!("compile status: {:?}", compiled.Status()?);

    recognizer.StateChanged(&TypedEventHandler::new(
        |_: Ref<SpeechRecognizer>, a: Ref<SpeechRecognizerStateChangedEventArgs>| {
            if let Ok(a) = a.ok() {
                println!("  state: {:?}", a.State()?);
            }
            Ok(())
        },
    ))?;
    recognizer.RecognitionQualityDegrading(&TypedEventHandler::new(
        |_: Ref<SpeechRecognizer>, a: Ref<SpeechRecognitionQualityDegradingEventArgs>| {
            if let Ok(a) = a.ok() {
                println!("  audio problem: {:?}", a.Problem()?);
            }
            Ok(())
        },
    ))?;
    recognizer.HypothesisGenerated(&TypedEventHandler::new(
        |_: Ref<SpeechRecognizer>, a: Ref<SpeechRecognitionHypothesisGeneratedEventArgs>| {
            if let Ok(a) = a.ok() {
                println!("  guess: {}", a.Hypothesis()?.Text()?);
            }
            Ok(())
        },
    ))?;
    let session = recognizer.ContinuousRecognitionSession()?;
    session.ResultGenerated(&TypedEventHandler::new(
        |_: Ref<SpeechContinuousRecognitionSession>,
         a: Ref<SpeechContinuousRecognitionResultGeneratedEventArgs>| {
            if let Ok(a) = a.ok() {
                let r = a.Result()?;
                println!(
                    "  RESULT: \"{}\" confidence {:?} status {:?}",
                    r.Text()?,
                    r.Confidence()?,
                    r.Status()?
                );
            }
            Ok(())
        },
    ))?;
    session.Completed(&TypedEventHandler::new(
        |_: Ref<SpeechContinuousRecognitionSession>,
         a: Ref<SpeechContinuousRecognitionCompletedEventArgs>| {
            if let Ok(a) = a.ok() {
                println!("  session ended: {:?}", a.Status()?);
            }
            Ok(())
        },
    ))?;
    session.StartAsync()?.join()?;
    println!("listening for 25 s — say \"Archy clip that\" a few times...");
    std::thread::sleep(std::time::Duration::from_secs(25));
    let _ = session.CancelAsync().map(|o| o.join());
    println!("done");
    Ok(())
}
