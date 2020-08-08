// Copyright (c) Microsoft. All rights reserved.

#![deny(rust_2018_idioms, warnings)]
#![deny(clippy::all, clippy::pedantic)]
#![allow(
	clippy::default_trait_access,
	clippy::missing_errors_doc,
	clippy::module_name_repetitions,
	clippy::must_use_candidate,
	clippy::too_many_lines,
)]
#![allow(dead_code)]

pub mod auth;
pub mod app;
pub mod error;
pub mod identity;
mod logging;
pub mod settings;

pub use error::Error;

/// This is the interval at which to poll DPS for registration assignment status
const DPS_ASSIGNMENT_RETRY_INTERVAL_SECS: u64 = 10;

/// This is the number of seconds to wait for DPS to complete assignment to a hub
const DPS_ASSIGNMENT_TIMEOUT_SECS: u64 = 120;

pub struct Server {
	pub settings: settings::Settings,
	pub authenticator: Box<dyn auth::authentication::Authenticator<Error = Error>  + Send + Sync>,
	pub authorizer: Box<dyn auth::authorization::Authorizer<Error = Error>  + Send + Sync>,
	pub id_manager: identity::IdentityManager,
}

impl Server {
	pub fn new(
		settings: settings::Settings, authenticator: Box<dyn auth::authentication::Authenticator<Error = Error>  + Send + Sync>, authorizer: Box<dyn auth::authorization::Authorizer<Error = Error>  + Send + Sync>) -> Result<Self, Error>
	{
		match settings.clone().provisioning.provisioning {
			settings::ProvisioningType::Manual { iothub_hostname, device_id, authentication } => {
				let credentials = match authentication {
					settings::ManualAuthMethod::SharedPrivateKey { device_id_pk } => aziot_identity_common::Credentials::SharedPrivateKey(device_id_pk),
					settings::ManualAuthMethod::X509 { identity_cert, identity_pk } => aziot_identity_common::Credentials::X509{identity_cert, identity_pk}
				};
				let id_manager = identity::IdentityManager::new(
					Some(aziot_identity_common::IoTHubDevice{
						iothub_hostname,
						device_id,
						credentials
					}
					));
				Ok(Server {
					settings,
					authenticator,
					authorizer,
					id_manager,
				})
			},
			settings::ProvisioningType::Dps { global_endpoint:_, scope_id:_, attestation:_} => {	
				Ok(Server {
					settings,
					authenticator,
					authorizer,
					id_manager: identity::IdentityManager::default(),
				})
			}
		}
	}

	pub async fn get_caller_identity(&self, auth_id: auth::AuthId) -> Result<aziot_identity_common::Identity, Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::GetDevice })? {
			return Err(Error::Authorization);
		}

		self.id_manager.get_device_identity().await
	}
	pub async fn get_identity(&self, auth_id: auth::AuthId, _idtype: &str, module_id: &str) -> Result<aziot_identity_common::Identity, Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::GetModule(String::from(module_id)) })? {
			return Err(Error::Authorization);
		}

		self.id_manager.get_module_identity(module_id).await
	}

	pub async fn get_hub_identities(&self, auth_id: auth::AuthId, _idtype: &str) -> Result<Vec<aziot_identity_common::Identity>, Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::GetAllHubModules })? {
			return Err(Error::Authorization);
		}

		//TODO: get identity type and get identities, filtering for appropriate identity manager (Hub or local)
		self.id_manager.get_module_identities().await
	}

	pub async fn get_device_identity(&self, auth_id: auth::AuthId, _idtype: &str) -> Result<aziot_identity_common::Identity, Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::GetDevice })? {
			return Err(Error::Authorization);
		}

		self.id_manager.get_device_identity().await
	}

	pub async fn create_identity(&self, auth_id: auth::AuthId, _idtype: &str, module_id: &str) -> Result<aziot_identity_common::Identity, Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::CreateModule(String::from(module_id)) })? {
			return Err(Error::Authorization);
		}

		//TODO: match identity type based on uid configuration and create and get identity from appropriate identity manager (Hub or local)
		self.id_manager.create_module_identity(module_id).await
	}

	pub async fn delete_identity(&self, auth_id: auth::AuthId, _idtype: &str, module_id: &str) -> Result<(), Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::DeleteModule(String::from(module_id)) })? {
			return Err(Error::Authorization);
		}

		//TODO: match identity type based on uid configuration and create and get identity from appropriate identity manager (Hub or local)
		self.id_manager.delete_module_identity(module_id).await
	}

	pub async fn get_trust_bundle(&self, auth_id: auth::AuthId) -> Result<aziot_cert_common_http::Pem, Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::GetTrustBundle })? {
			return Err(Error::Authorization);
		}

		//TODO: invoke get trust bundle
		Ok(aziot_cert_common_http::Pem { 0: std::vec::Vec::default() })
	}

	pub async fn reprovision_device(&mut self, auth_id: auth::AuthId) -> Result<(), Error> {

		if !self.authorizer.authorize(auth::Operation{auth_id, op_type: auth::OperationType::ReprovisionDevice })? {
			return Err(Error::Authorization);
		}

		self.provision_device().await
	}

	pub async fn provision_device(&mut self) -> Result<(), Error> {
		match self.settings.clone().provisioning.provisioning {
			settings::ProvisioningType::Manual { iothub_hostname, device_id, authentication } => {
				match authentication {
					settings::ManualAuthMethod::SharedPrivateKey { device_id_pk } => {
						let credentials = aziot_identity_common::Credentials::SharedPrivateKey(device_id_pk);
						self.id_manager.set_device(aziot_identity_common::IoTHubDevice { iothub_hostname, device_id, credentials });
					},
					settings::ManualAuthMethod::X509 { identity_cert, identity_pk } => { 
						let credentials = aziot_identity_common::Credentials::X509{identity_cert, identity_pk};
						self.id_manager.set_device(aziot_identity_common::IoTHubDevice { iothub_hostname, device_id, credentials });
					}
				}
			},
			settings::ProvisioningType::Dps { global_endpoint, scope_id, attestation} => {
				match attestation {
					settings::DpsAttestationMethod::SymmetricKey { registration_id, symmetric_key } => {
						let result = aziot_dps_client_async::register(
							global_endpoint.as_str(), 
							&scope_id, 
							&registration_id,
							Some(symmetric_key.clone()),
							None,
							None).await;
						let device = match result
						{
							Ok(operation) => {
								let mut retry_count = (DPS_ASSIGNMENT_TIMEOUT_SECS / DPS_ASSIGNMENT_RETRY_INTERVAL_SECS) + 1;
								let credential = aziot_identity_common::Credentials::SharedPrivateKey(symmetric_key.clone());
								loop {
									if retry_count == 0 {
										return Err(Error::DeviceNotFound);
									}
									let credential_clone = credential.clone();
									let result = aziot_dps_client_async::get_operation_status(
										global_endpoint.as_str(), 
										&scope_id, 
										&registration_id,
										&operation.operation_id,
										Some(symmetric_key.clone()),
										None,
										None).await;

									match result {
										Ok(reg_status) => {
											match reg_status.status {
												Some(status) => {
													if !status.eq_ignore_ascii_case("assigning") {
														let mut state = reg_status.registration_state.ok_or(Error::DeviceNotFound)?;
															let iothub_hostname = state.assigned_hub.get_or_insert("".into());
															let device_id = state.device_id.get_or_insert("".into());
															let device = aziot_identity_common::IoTHubDevice { 
																iothub_hostname: iothub_hostname.clone(), 
																device_id: device_id.clone(),
																credentials: credential_clone };
														
														break device;
													}
												}
												None => { return Err(Error::DeviceNotFound) }
											};
										},
										Err(err) => { return Err(Error::DPSClient(err)) }
									}
									retry_count -= 1;
									tokio::time::delay_for(tokio::time::Duration::from_secs(DPS_ASSIGNMENT_RETRY_INTERVAL_SECS)).await;
								}
							},
							Err(err) => return Err(Error::DPSClient(err)) 
						};
						
						self.id_manager.set_device(device);
					},
					settings::DpsAttestationMethod::X509 { registration_id, identity_cert, identity_pk } => {
						let result = aziot_dps_client_async::register(
							global_endpoint.as_str(), 
							&scope_id, 
							&registration_id,
							None,
							Some(identity_cert.clone()),
							Some(identity_pk.clone())).await;

						let device = match result
						{
							Ok(operation) => {
								let mut retry_count = (DPS_ASSIGNMENT_TIMEOUT_SECS / DPS_ASSIGNMENT_RETRY_INTERVAL_SECS) + 1;
								let credential = aziot_identity_common::Credentials::X509{identity_cert: identity_cert.clone(), identity_pk: identity_pk.clone()};
								loop {
									if retry_count == 0 {
										return Err(Error::DeviceNotFound);
									}
									let credential_clone = credential.clone();
									let result = aziot_dps_client_async::get_operation_status(
										global_endpoint.as_str(), 
										&scope_id, 
										&registration_id,
										&operation.operation_id,
										None,
										Some(identity_cert.clone()),
										Some(identity_pk.clone())).await;

									match result {
										Ok(reg_status) => {
											match reg_status.status {
												Some(status) => {
													if !status.eq_ignore_ascii_case("assigning") {
														let mut state = reg_status.registration_state.ok_or(Error::DeviceNotFound)?;
															let iothub_hostname = state.assigned_hub.get_or_insert("".into());
															let device_id = state.device_id.get_or_insert("".into());
															let device = aziot_identity_common::IoTHubDevice { 
																iothub_hostname: iothub_hostname.clone(), 
																device_id: device_id.clone(),
																credentials: credential_clone };
														
														break device;
													}
												}
												None => { return Err(Error::DeviceNotFound) }
											};
										},
										Err(_) => {return Err(Error::DeviceNotFound) }
									}
									retry_count -= 1;
									tokio::time::delay_for(tokio::time::Duration::from_secs(DPS_ASSIGNMENT_RETRY_INTERVAL_SECS)).await;
								}
							},
							Err(_) => return Err(Error::DeviceNotFound)
						};
						
						self.id_manager.set_device(device);
					}
				}
			}
		};
		Ok(())
	}
}

pub struct SettingsAuthorizer {
	pub pmap: std::collections::BTreeMap<crate::auth::Uid, crate::settings::Principal>,
	pub mset: std::collections::BTreeSet<aziot_identity_common::ModuleId>,
}

impl auth::authorization::Authorizer for SettingsAuthorizer
{
	type Error = Error;

	fn authorize(&self, o: auth::Operation) -> Result<bool, Self::Error> {
		match o.op_type {
			crate::auth::OperationType::GetModule(m) => {
				if let crate::auth::AuthId::LocalPrincipal(creds) = o.auth_id {
					if let Some(p) = self.pmap.get(&crate::auth::Uid(creds.0)) {
						return Ok(p.name.0 == m && p.id_type == Some(aziot_identity_common::IdType::Module))
					}
				}
			},
			crate::auth::OperationType::GetDevice => {
				if let crate::auth::AuthId::LocalPrincipal(creds) = o.auth_id {
					if let Some(p) = self.pmap.get(&crate::auth::Uid(creds.0)) {
						return Ok(p.id_type == None || p.id_type == Some(aziot_identity_common::IdType::Device))
					}
				}
			},
			crate::auth::OperationType::CreateModule(m) |
			crate::auth::OperationType::DeleteModule(m) => {
				if let crate::auth::AuthId::LocalPrincipal(creds) = o.auth_id {
					if let Some(p) = self.pmap.get(&crate::auth::Uid(creds.0)) {
						return Ok(p.id_type == None && !self.mset.contains(&aziot_identity_common::ModuleId(m)))
					}
				}
			},
			crate::auth::OperationType::GetTrustBundle => {
				return Ok(true)
			},
			crate::auth::OperationType::GetAllHubModules |
			crate::auth::OperationType::ReprovisionDevice => {
				if let crate::auth::AuthId::LocalPrincipal(creds) = o.auth_id {
					if let Some(p) = self.pmap.get(&crate::auth::Uid(creds.0)) {
						return Ok(p.id_type == None)
					}
				}
			},
		}
		Ok(false)
	}
}

fn test_module_identity() -> aziot_identity_common::Identity {
	aziot_identity_common::Identity::Aziot (
		aziot_identity_common::AzureIoTSpec {
			hub_name: "dummyHubName".to_string(),
			device_id: aziot_identity_common::DeviceId("dummyDeviceId".to_string()),
			module_id: Some(aziot_identity_common::ModuleId("dummyModuleId".to_string())),
			gen_id: Some(aziot_identity_common::GenId("dummyGenId".to_string())),
			auth: None,
			})
}
