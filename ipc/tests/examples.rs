// Copyright 2015, 2016 Ethcore (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

#[cfg(test)]
mod tests {

	use super::super::service::*;
	use ipc::*;
	use devtools::*;

	#[test]
	fn call_service() {
		// method_num = 0, f = 10 (method Service::commit)
		let mut socket = TestSocket::new_ready(vec![0, 0, 0, 0, 0, 10]);

		let service = Service::new();
		assert_eq!(0, *service.commits.read().unwrap());

		service.dispatch(&mut socket);

		assert_eq!(10, *service.commits.read().unwrap());
	}

	#[test]
	fn call_service_proxy() {
		let mut socket = TestSocket::new();
		socket.read_buffer = vec![0, 0, 0, 10];
		let service_client = ServiceClient::new(socket);

		let result = service_client.commit(5);

		assert_eq!(vec![0, 0, 0, 0, 0, 5], service_client.socket().borrow().write_buffer.clone());
		assert_eq!(10, result);
	}

	#[test]
	fn call_service_proxy_optional() {
		let mut socket = TestSocket::new();
		socket.read_buffer = vec![0, 0, 0, 10];
		let service_client = ServiceClient::new(socket);

		let result = service_client.rollback(Some(5), 10);

		assert_eq!(vec![0, 1, 1, 0, 0, 0, 5, 0, 0, 0, 10], service_client.socket().borrow().write_buffer.clone());
		assert_eq!(10, result);
	}
}
