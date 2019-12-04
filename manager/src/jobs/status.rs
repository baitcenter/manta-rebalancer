/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

use super::evacuate::EvacuateObjectStatus;
use crate::pg_db;
use rebalancer::error::Error;

use std::collections::HashMap;
use std::str::FromStr;
use std::string::ToString;

use diesel::prelude::*;
use diesel::result::ConnectionError;
use field_count::FieldCount;
use inflector::cases::titlecase::to_title_case;
use strum::IntoEnumIterator;
use uuid::Uuid;
use crate::jobs::evacuate::EvacuateObject;

#[derive(Debug, EnumString)]
pub enum StatusError {
    DBExists,
    LookupError,
    Unknown,
}

fn status_db_conn(uuid: Uuid) -> Result<PgConnection, StatusError> {
    let db_name = uuid.to_string();
    let conn = match pg_db::connect_db(&db_name) {
        Ok(c) => c,
        Err(e) => {
            if let Error::DieselConnection(conn_err) = &e {
                if let ConnectionError::BadConnection(err) = conn_err {
                    error!("Status DB connection: {}", err);
                    return Err(StatusError::DBExists);
                }
            }

            error!("Unknown status DB connection error: {}", e);
            return Err(StatusError::Unknown);
        }
    };

    Ok(conn)
}
pub fn get_status(uuid: Uuid) -> Result<HashMap<String, usize>, StatusError> {
    use super::evacuate::evacuateobjects::dsl::*;
    let mut total_count = 0;
    let mut ret = HashMap::new();

    let conn = status_db_conn(uuid)?;

    let status_vec: Vec<EvacuateObjectStatus> =
        match evacuateobjects.select(status).get_results(&conn) {
            Ok(res) => res,
            Err(e) => {
                error!("Status DB query: {}", e);
                return Err(StatusError::LookupError);
            }
        };

    for status_value in EvacuateObjectStatus::iter() {
        let count = status_vec.iter().filter(|s| *s == &status_value).count();
        let status_str = to_title_case(&status_value.to_string());

        ret.insert(status_str, count);
        total_count += count;
    }

    ret.insert("Total".into(), total_count);

    Ok(ret)
}

pub fn list_jobs() -> Result<Vec<String>, StatusError> {
    let db_list = match pg_db::list_databases() {
        Ok(list) => list,
        Err(e) => {
            error!("Error listing jobs: {}", e);
            return Err(StatusError::Unknown);
        }
    };
    let mut ret = vec![];

    for db in db_list {
        if let Ok(job_id) = Uuid::from_str(&db) {
            ret.push(job_id.to_string());
        }
    }

    Ok(ret)
}

// https://github.com/diesel-rs/diesel/blob/master/examples/postgres/advanced-blog-cli/src/pagination.rs
// http://diesel.rs/guides/extending-diesel/
pub struct ObjectAndStatus {
    uuid: String,
    status: EvacuateObjectStatus,
    reason: String
}

pub fn list_objects_and_status(uuid: Uuid)
    -> Result<Vec<ObjectAndStatus>, StatusError>
{
    use super::evacuate::evacuateobjects::dsl::*;
    let conn = status_db_conn(uuid)?;
    let mut ret = vec![];

    let objs = evacuateobjects.load::<EvacuateObject>(&conn).expect("todo");

    for obj in objs {
        let reason = match obj.status {
            EvacuateObjectStatus::Skipped => {
                obj.skipped_reason.unwrap().to_string()
            },
            EvacuateObjectStatus::Error => {
                obj.error.unwrap().to_string()
            },
            _ => String::from("")
        };

        ret.push(ObjectAndStatus {
            uuid: obj.id,
            status: obj.status,
            reason
        });
    }

    Ok(ret)
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::evacuate::{self, EvacuateObject};
    use crate::pg_db;
    use quickcheck::{quickcheck, Arbitrary, Gen, StdThreadGen};
    use rand::Rng;
    use rebalancer::util;

    static NUM_OBJS: u32 = 200;

    #[test]
    fn big_list() {
        use crate::jobs::evacuate::evacuateobjects::dsl::*;

        let _guard = util::init_global_logger();
        let uuid = Uuid::new_v4();
        let mut g = StdThreadGen::new(10);
        let mut obj_vec = vec![];

        let conn = pg_db::create_and_connect_db(&uuid.to_string()).unwrap();
        evacuate::create_evacuateobjects_table(&conn).unwrap();

        for _ in 0..1_000 {
            obj_vec.push(EvacuateObject::arbitrary(&mut g));
        }

        let chunk_size = 65535 / EvacuateObject::field_count();
        let insert_vecs: Vec<&[EvacuateObject]> = obj_vec.chunks(chunk_size + 1)
            .collect();
        for sub_vec in insert_vecs {
            diesel::insert_into(evacuateobjects)
                .values(sub_vec.clone())
                .execute(&conn)
                .unwrap();
        }

        //////////////
        use diesel::associations::HasTable;
        use diesel::{sql_query, debug_query};
        use diesel::pg::Pg;
        use crate::pagination::*;
        let mut query = evacuateobjects.select(evacuateobjects::all_columns()
        ).paginate(5).per_page(100);

        let sql = debug_query::<Pg, _>(&query).to_string();
        dbg!(sql);

        let ret = query.load_and_count_pages::<EvacuateObject>(&conn).unwrap();
        dbg!(ret);
        //////////////

        /*
        diesel::insert_into(evacuateobjects)
            .values(obj_vec.clone())
            .execute(&conn)
            .unwrap();
            */

        /*
        if let Err(_) = diesel::insert_into(evacuateobjects)
            .values(obj_vec.clone())
            .execute(&conn) {
                return false;
        }
        */


        list_objects_and_status(uuid).unwrap();
        /*
        if let Err(_) = list_objects_and_status(uuid) {
            return false;
        }
        true
        */

    }


    #[test]
    fn list_job_test() {
        assert!(list_jobs().is_ok());
    }

    #[test]
    fn bad_job_id() {
        let _guard = util::init_global_logger();
        let uuid = Uuid::new_v4();
        assert!(get_status(uuid).is_err());
    }

    #[test]
    fn get_status_test() {
        use crate::jobs::evacuate::evacuateobjects::dsl::*;

        let _guard = util::init_global_logger();
        let uuid = Uuid::new_v4();
        let mut g = StdThreadGen::new(10);
        let mut obj_vec = vec![];

        let conn = pg_db::create_and_connect_db(&uuid.to_string()).unwrap();
        evacuate::create_evacuateobjects_table(&conn).unwrap();

        for _ in 0..NUM_OBJS {
            obj_vec.push(EvacuateObject::arbitrary(&mut g));
        }

        diesel::insert_into(evacuateobjects)
            .values(obj_vec.clone())
            .execute(&conn)
            .unwrap();

        get_status(uuid).unwrap();
    }
}
