//! Framework restart ordering, independent of Android IPC so failures are testable.
use anyhow::{Context, Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step<'a> {
    Stop(&'a str),
    Prepare,
    QuiesceModules,
    PostFsData,
    Start(&'a str),
    Services,
    WaitBoot,
    BootCompleted,
}

/// Include auxiliary ART services (secondary, prewarmed/next slots), but never
/// native services. Stop the primary last and resume it first.
pub fn running_services(properties: &[(String, String)]) -> Result<Vec<&str>> {
    let mut services = Vec::new();
    for (name, state) in properties {
        let Some(service) = name.strip_prefix("init.svc.") else {
            continue;
        };
        if service != "zygote" && !service.starts_with("zygote_") {
            continue;
        }
        match state.as_str() {
            "running" => services.push(service),
            "stopped" => {}
            _ => bail!("Android runtime service {service} is not stable: {state}"),
        }
    }
    ensure!(
        services.contains(&"zygote"),
        "Android zygote is not running"
    );
    services.sort_unstable_by_key(|service| (*service == "zygote", *service));
    Ok(services)
}

/// `services` contains only running ART services, primary last.
/// Explicit init stops hold them down; a signal alone lets init race module activation.
pub fn restart<'a>(
    services: &[&'a str],
    mut perform: impl FnMut(Step<'a>) -> Result<()>,
) -> Result<()> {
    let activation = (|| {
        for service in services {
            perform(Step::Stop(service))?;
        }
        perform(Step::Prepare)?;
        perform(Step::QuiesceModules)?;
        perform(Step::PostFsData)?;
        perform(Step::Services)
    })();

    // Even a failed stop may have reached init. Always attempt every original service.
    let mut resumed = Ok(());
    for service in services.iter().rev() {
        if let Err(error) = perform(Step::Start(service)) {
            resumed = Err(error);
        }
    }
    resumed.context("Failed to resume Android runtime")?;
    activation?;
    perform(Step::WaitBoot)?;
    perform(Step::BootCompleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::collections::BTreeSet;

    #[test]
    fn modules_activate_only_while_all_runtimes_are_held_down() {
        for services in [
            vec!["zygote"],
            vec!["zygote_secondary", "zygote"],
            vec!["zygote_next", "zygote_secondary", "zygote"],
        ] {
            let mut running: BTreeSet<_> = services.iter().copied().collect();
            let mut events = Vec::new();
            restart(&services, |step| {
                events.push(step);
                match step {
                    Step::Stop(service) => {
                        running.remove(service);
                    }
                    Step::Prepare | Step::QuiesceModules | Step::PostFsData | Step::Services => {
                        assert!(running.is_empty());
                    }
                    Step::Start(service) => {
                        running.insert(service);
                    }
                    _ => assert_eq!(running.len(), services.len()),
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(
                &events[events.len() - 2..],
                &[Step::WaitBoot, Step::BootCompleted]
            );
            assert!(
                events.iter().position(|e| *e == Step::Services).unwrap()
                    < events
                        .iter()
                        .position(|e| matches!(e, Step::Start(_)))
                        .unwrap()
            );
            assert!(
                events
                    .iter()
                    .position(|e| *e == Step::QuiesceModules)
                    .unwrap()
                    < events.iter().position(|e| *e == Step::PostFsData).unwrap()
            );
        }
    }

    #[test]
    fn discovers_all_running_art_services_and_rejects_unstable_runtimes() {
        let mut properties: Vec<_> = [
            ("init.svc.zygote", "running"),
            ("init.svc.zygote_secondary", "stopped"),
            ("init.svc.zygote_next", "running"),
            ("init.svc.surfaceflinger", "running"),
            ("init.svc.vendor.composer", "restarting"),
            ("ro.zygote", "zygote64"),
        ]
        .map(|(name, state)| (name.to_owned(), state.to_owned()))
        .into();
        assert_eq!(
            running_services(&properties).unwrap(),
            ["zygote_next", "zygote"]
        );
        properties[1].1 = "restarting".into();
        assert!(running_services(&properties).is_err());
        properties[1].1 = "stopped".into();
        properties[0].1 = "stopped".into();
        assert!(running_services(&properties).is_err());
        assert!(running_services(&[]).is_err());
    }

    #[test]
    fn failures_resume_every_runtime_and_never_report_boot_complete() {
        for failure in [
            Step::Stop("zygote_secondary"),
            Step::Stop("zygote"),
            Step::Prepare,
            Step::QuiesceModules,
            Step::PostFsData,
            Step::Start("zygote"),
            Step::Start("zygote_secondary"),
            Step::Services,
            Step::WaitBoot,
        ] {
            let mut events = Vec::new();
            let result = restart(&["zygote_secondary", "zygote"], |step| {
                events.push(step);
                if step == failure {
                    bail!("injected failure");
                }
                Ok(())
            });
            assert!(result.is_err(), "{failure:?}");
            assert!(events.contains(&Step::Start("zygote")), "{failure:?}");
            assert!(
                events.contains(&Step::Start("zygote_secondary")),
                "{failure:?}"
            );
            assert!(!events.contains(&Step::BootCompleted), "{failure:?}");
            if matches!(
                failure,
                Step::Stop(_) | Step::Prepare | Step::QuiesceModules
            ) {
                assert!(!events.contains(&Step::PostFsData));
            }
        }
    }
}
