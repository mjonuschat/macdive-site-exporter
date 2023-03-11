use crate::inaturalist::{TaxonCategoryName, TaxonGroupName};
use crate::macdive;
use crate::macdive::models::CritterUpdate;
use crate::types::CritterCategoryOverride;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub async fn diff_critters(database: &Path) -> anyhow::Result<()> {
    let connection = macdive::establish_connection(database).await?;
    let critters = crate::macdive::critters(&connection).await?;

    let species = critters
        .iter()
        .filter_map(|c| c.species.as_deref())
        .collect::<Vec<_>>();

    crate::inaturalist::cache_species(&species).await?;

    for critter in critters {
        if let Some(scientific_name) = critter.species.as_deref() {
            tracing::trace!("Looking up {scientific_name} on iNaturalist");
            let taxon = match crate::inaturalist::get_taxon_by_name(scientific_name).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        scientific_name = scientific_name,
                        "Failed to retrieve taxon: {e}"
                    );
                    continue;
                }
            };

            let current_name = critter
                .name
                .as_deref()
                .map(|v| change_case::title_case(v.trim()));
            let preferred_name = taxon
                .preferred_common_name
                .as_deref()
                .map(|v| change_case::title_case(v.trim()));

            let scientific_name = change_case::title_case(scientific_name);

            let mut changeset: CritterUpdate = CritterUpdate {
                id: critter.id,
                ..Default::default()
            };

            if let Some(preferred_scientific_name) = taxon.name.as_deref() {
                let preferred_scientific_name =
                    change_case::sentence_case(preferred_scientific_name);
                let current_scientific_name = change_case::sentence_case(&scientific_name);

                if current_scientific_name != preferred_scientific_name {
                    println!(
                        "Mismatched scientific name: MacDive: {} => iNat: {}",
                        current_scientific_name, preferred_scientific_name
                    );
                    changeset.scientific_name = Some(preferred_scientific_name);
                }
            }

            match (current_name, preferred_name) {
                (Some(current_name), Some(preferred_name)) if preferred_name != current_name => {
                    // TODO: Make this a nice table
                    println!(
                        "Mismatched common name: MacDive {:?} => iNat: {:?}",
                        &current_name, &preferred_name
                    );
                    changeset.common_name = Some(preferred_name);
                }
                (None, Some(preferred_name)) => {
                    println!(
                        "Found new common name for {}: {}",
                        &scientific_name, &preferred_name
                    );
                    changeset.common_name = Some(preferred_name);
                }
                (Some(_), Some(_)) => {
                    // Pass, names are identical
                }
                (Some(_), None) => {
                    // Pass, no registered common name in iNaturalist
                }
                (None, None) => {
                    println!("Woha, no common name for species: {}", &scientific_name)
                }
            }

            // TODO: Guard with command line flag!
            // if changeset.has_changes() {
            //     crate::macdive::update_critter(&changeset, &connection).await?;
            // }
        }
    }
    Ok(())
}

pub async fn diff_critter_categories(
    database: &Path,
    overrides: &CritterCategoryOverride,
) -> anyhow::Result<()> {
    let connection = macdive::establish_connection(database).await?;

    let critters = crate::macdive::critters(&connection).await?;

    // Categories that currently are in MacDive
    let mut current_categories = crate::macdive::critter_categories(&connection)
        .await?
        .into_iter()
        .filter_map(|category| match category.name.as_deref() {
            Some(name) => {
                let key = change_case::lower_case(name);
                Some((key, category))
            }
            None => None,
        })
        .collect::<HashMap<_, _>>();

    let critter_groups: HashMap<String, TaxonGroupName> =
        futures::stream::iter(critters.iter().filter_map(|c| c.species.clone()))
            .filter_map(|scientific_name| async move {
                if let Ok(taxon) = crate::inaturalist::get_taxon_by_name(&scientific_name).await {
                    if let Ok(group_name) = taxon.group_name(overrides).await {
                        return Some((scientific_name, group_name));
                    }
                } else {
                    tracing::error!(
                        scientific_name = scientific_name.as_str(),
                        "Taxon lookup failed"
                    )
                }

                None
            })
            .collect()
            .await;

    let current_names: HashSet<String> = current_categories
        .keys()
        .map(|v| change_case::lower_case(v))
        .collect();

    let desired_names: HashSet<String> = critter_groups
        .values()
        .map(|v| change_case::lower_case(&v.to_string()))
        .collect();

    let mut extraneous_categories: Vec<String> = current_names
        .difference(&desired_names)
        .map(|v| v.to_owned())
        .collect();

    let mut category_index: HashMap<_, _> = current_categories
        .iter()
        .map(|(k, v)| (v.id, k.to_owned()))
        .collect();

    for critter in critters {
        if let Some(scientific_name) = &critter.species {
            let current_category = &critter.category.and_then(|id| {
                category_index
                    .get(&id)
                    .and_then(|key| current_categories.get(key))
            });
            let desired_category = &critter_groups
                .get(scientific_name)
                .and_then(|v| current_categories.get(&change_case::lower_case(&v.to_string())));

            match (current_category, desired_category) {
                (Some(cc), Some(dc)) if cc.id != dc.id => {
                    // TODO: Delta
                    eprintln!(
                        "Re-Assigning: {:?} ({:?}): {:?} => {:?}",
                        &critter.name, &critter.species, &cc.name, &dc.name
                    );
                    // crate::macdive::update_critter(
                    //     &CritterUpdate {
                    //         id: critter.id,
                    //         category: Some(dc.id),
                    //         common_name: critter.name,
                    //         ..Default::default()
                    //     },
                    //     &connection,
                    // )
                    // .await?;
                }
                (Some(_), Some(_)) => {
                    // Old and new category are identical
                }
                (None, Some(dc)) => {
                    eprintln!(
                        "Assigning: {:?} ({:?}): --- => {:?}",
                        &critter.name, &critter.species, &dc.name
                    );
                    // crate::macdive::update_critter(
                    //     &CritterUpdate {
                    //         id: critter.id,
                    //         category: Some(dc.id),
                    //         common_name: critter.name,
                    //         ..Default::default()
                    //     },
                    //     &connection,
                    // )
                    // .await?;
                }
                (Some(_cc), None) => match &critter_groups.get(scientific_name) {
                    Some(new_category) => {
                        let category = extraneous_categories
                            .pop()
                            .and_then(|key| current_categories.remove(&key));

                        match category {
                            Some(mut c) => {
                                let old_name = c.name.clone();
                                let new_name = new_category.to_string();
                                c.name = Some(new_name.clone());
                                let key = change_case::lower_case(&new_category.to_string());
                                let id = c.id;

                                current_categories.insert(key.clone(), c);
                                category_index.insert(id, key);

                                eprintln!(
                                    "Renaming category {:?} => {:?}",
                                    old_name,
                                    new_category.to_string()
                                );
                                eprintln!(
                                    "Re-Assigning: {:?} ({:?}): {:?} => {:?}",
                                    &critter.name, &critter.species, &old_name, &new_name
                                );

                                // crate::macdive::update_critter_category(
                                //     id,
                                //     &change_case::title_case(&new_category.to_string()),
                                //     &connection,
                                // )
                                // .await?;
                                //
                                // crate::macdive::update_critter(
                                //     &CritterUpdate {
                                //         id: critter.id,
                                //         category: Some(id),
                                //         common_name: critter.name,
                                //         ..Default::default()
                                //     },
                                //     &connection,
                                // )
                                // .await?;
                            }
                            None => {
                                eprintln!("Brand spanking new category needed: {}", new_category)
                            }
                        }
                    }
                    None => eprintln!(
                        "This should not happen - no new category: {}",
                        scientific_name
                    ),
                },
                (None, None) => {
                    let new_category = &critter_groups.get(scientific_name).unwrap();
                    eprintln!("New category required [2]: {}", new_category);
                }
            }
        }
    }
    // println!("Missing categories: {:#?}", &missing);
    println!("Extraneous categories: {:#?}", &extraneous_categories);
    // println!("Existing categories: {:#?}", &existing);

    Ok(())
}
